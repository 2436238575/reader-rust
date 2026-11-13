//! 任务子系统（P2/D13）：单进程内存注册表 + 每任务事件环形缓冲 + 结果集。
//!
//! **进程重启丢任务**（注册表是内存表），客户端按 404 处理并重发任务即可；
//! 任务 id 的序号以随机的 64 位基数起算，重启后旧 id 不会指向新任务。
//!
//! 资源上限（防单用户打爆内存）：
//! - 注册表最多 64 个任务（含已结束未过期的），按结束时间驱逐；
//! - 每任务结果最多 10MB，全局结果字节预算 128MB（超预算先驱逐最老的已结束任务，
//!   仍不够则拒绝新任务/标 `truncated`）；
//! - 每任务事件环最多 256 条且 64KB（逐出最旧事件）；
//! - 每任务最多 4 个并发 SSE 订阅者。
//!
//! 结果与任务保留 24 小时，但容量压力下会**提前驱逐**已结束任务（GET 得到 404）。
//!
//! 事件流（SSE）不是唯一真相：客户端重连先 `GET /v1/tasks/{id}` 拿全量状态，
//! 再用 `Last-Event-ID` 补齐增量。事件 id 在单任务内单调递增，环形缓冲之外
//! 的 id 只重放保留后缀（客户端可用结果分页对账）。
//!
//! 取消是协作式的：`request_cancel` 置位并唤醒工作器，工作器在下一轮调度与
//! `select!` 等待中立即察觉，abort 在飞任务后把状态落到 `cancelled`。
//! `finish` 会尊重已发出的取消请求（`cancelling` + 非 `cancelled` 终态 → 强制
//! `cancelled`），并有 5 秒 watchdog 兜底卡死的工作器。

use crate::api::problem::{ErrorCode, Problem};
use crate::model::search::SearchBook;
use crate::util::time::{now_ts, rfc3339_secs};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, Notify};

/// 全局并发任务上限（契约 §7 / 限流表：8）。
pub const GLOBAL_TASK_LIMIT: usize = 8;
/// 每用户并发任务上限（契约 §7：4）。
pub const PER_USER_TASK_LIMIT: usize = 4;
/// 每任务事件环形缓冲条数上限。
pub const EVENT_RING_CAPACITY: usize = 256;
/// 每任务事件环形缓冲字节上限（约容纳 8 个 32KB 的结果块）。
pub const EVENT_RING_MAX_BYTES: usize = 256 * 1024;
/// 单结果集上限：10MB。
pub const RESULT_MAX_BYTES: usize = 10 * 1024 * 1024;
/// 全部任务结果集的进程内预算：128MB。
pub const GLOBAL_RESULT_BUDGET: usize = 128 * 1024 * 1024;
/// 任务与结果的保留时长：24 小时。
pub const RESULT_TTL_SECS: i64 = 24 * 60 * 60;
/// 注册表总条目上限（含已结束但未过期的任务）。
const REGISTRY_MAX_TASKS: usize = 64;
/// 每任务并发 SSE 订阅者上限。
const MAX_SUBSCRIBERS_PER_TASK: usize = 4;
/// 限流时建议的重试间隔（秒）。
const TASK_LIMIT_RETRY_AFTER_SECS: u64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TaskKind {
    Search,
    Explore,
    SourceTest,
    SourceImport,
    PrefetchCache,
    ChapterImport,
    AiGenerate,
    AiChat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TaskState {
    Queued,
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
}

impl TaskState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskProgress {
    pub done: u32,
    pub total: u32,
    /// 进度单位：`source`（搜索书源数）等
    pub unit: String,
    pub message: Option<String>,
}

impl TaskProgress {
    fn new(total: u32, unit: &str) -> Self {
        Self {
            done: 0,
            total,
            unit: unit.to_string(),
            message: None,
        }
    }
}

/// 任务资源（`GET /v1/tasks/{id}` 的响应）。
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskView {
    pub id: String,
    pub kind: TaskKind,
    pub state: TaskState,
    pub progress: TaskProgress,
    #[schema(format = DateTime)]
    pub created_at: String,
    #[schema(format = DateTime)]
    pub started_at: Option<String>,
    #[schema(format = DateTime)]
    pub finished_at: Option<String>,
    /// 结果分页地址；没有结果时为 null
    pub result_href: Option<String>,
    /// 失败时的完整 problem+json 对象
    pub error: Option<Problem>,
}

/// 任务列表（`GET /v1/tasks`）。
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskPage {
    pub items: Vec<TaskView>,
    pub next_cursor: Option<String>,
    pub total: u32,
}

/// 搜索结果分页（`GET /v1/tasks/{id}/result`）。
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskResultPage {
    pub items: Vec<SearchBook>,
    pub next_cursor: Option<String>,
    /// 当前结果集条数（去重后；截断后只算保留部分）
    pub total: u32,
    /// 结果集超过 10MB / 全局预算，后续条目已丢弃
    pub truncated: bool,
}

/// 一条已发生的事件。`data` 是序列化后的 JSON 文本（SSE 的 `data:` 行）。
#[derive(Debug, Clone)]
pub struct TaskEvent {
    pub id: u64,
    pub event: String,
    pub data: String,
}

/// 建立事件流时的快照：先重放 `replay`，再从 `receiver` 续读新事件。
pub struct TaskSubscription {
    pub replay: Vec<TaskEvent>,
    pub receiver: broadcast::Receiver<TaskEvent>,
    /// 建立订阅时任务是否已结束（重放完即可关闭流）
    pub terminal: bool,
    /// `Last-Event-ID` 早于环形缓冲起点（部分事件已不可重放）
    pub gap: bool,
}

struct Entry {
    owner_ns: String,
    /// 任务序号：id 的数字部分，列表排序/游标用（随机基数起算，重启不复用）
    seq: u64,
    view: TaskView,
    cancel: Arc<AtomicBool>,
    cancel_notify: Arc<Notify>,
    events: VecDeque<TaskEvent>,
    event_bytes: usize,
    next_event_id: u64,
    tx: broadcast::Sender<TaskEvent>,
    results: Vec<SearchBook>,
    dedupe_index: HashMap<String, usize>,
    result_bytes: usize,
    truncated: bool,
    finished_epoch: Option<i64>,
}

struct Inner {
    next_seq: u64,
    tasks: HashMap<String, Entry>,
    total_result_bytes: usize,
    /// `(owner_ns, Idempotency-Key)` → 任务 id（任务被 TTL/容量驱逐时一并清理）
    idempotency: HashMap<(String, String), String>,
}

pub struct TaskService {
    inner: Mutex<Inner>,
    global_limit: usize,
    per_user_limit: usize,
    event_capacity: usize,
    event_max_bytes: usize,
    result_max_bytes: usize,
    global_result_budget: usize,
    ttl_secs: i64,
}

impl Default for TaskService {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskService {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                next_seq: random_sequence_base(),
                tasks: HashMap::new(),
                total_result_bytes: 0,
                idempotency: HashMap::new(),
            }),
            global_limit: GLOBAL_TASK_LIMIT,
            per_user_limit: PER_USER_TASK_LIMIT,
            event_capacity: EVENT_RING_CAPACITY,
            event_max_bytes: EVENT_RING_MAX_BYTES,
            result_max_bytes: RESULT_MAX_BYTES,
            global_result_budget: GLOBAL_RESULT_BUDGET,
            ttl_secs: RESULT_TTL_SECS,
        }
    }

    /// 测试用：缩小上限/容量/结果字节数/保留期，避免真跑 24h 与 10MB。
    pub fn with_limits(
        global_limit: usize,
        per_user_limit: usize,
        event_capacity: usize,
        result_max_bytes: usize,
        ttl_secs: i64,
    ) -> Self {
        Self {
            inner: Mutex::new(Inner {
                next_seq: 0,
                tasks: HashMap::new(),
                total_result_bytes: 0,
                idempotency: HashMap::new(),
            }),
            global_limit,
            per_user_limit,
            event_capacity,
            event_max_bytes: EVENT_RING_MAX_BYTES,
            result_max_bytes,
            global_result_budget: GLOBAL_RESULT_BUDGET,
            ttl_secs,
        }
    }

    /// 只检查并发名额，不注册任务（参数解析/书源解析之前先挡一刀）。
    pub fn check_capacity(&self, owner_ns: &str) -> Result<(), Problem> {
        let mut inner = self.lock();
        prune(&mut inner, now_ts(), self.ttl_secs);
        self.check_active_capacity(&inner, owner_ns)
    }

    /// 注册任务（queued）。并发超限 → 429 `TASK_LIMIT` + `Retry-After`。
    pub fn create(&self, owner_ns: &str, kind: TaskKind, total: u32) -> Result<TaskView, Problem> {
        self.create_with_progress(owner_ns, kind, TaskProgress::new(total, "source"))
    }

    pub fn create_with_progress(
        &self,
        owner_ns: &str,
        kind: TaskKind,
        progress: TaskProgress,
    ) -> Result<TaskView, Problem> {
        self.create_inner(owner_ns, kind, progress, None)
            .map(|(view, _)| view)
    }

    /// 注册任务并原子绑定 `Idempotency-Key`：命中已有 key 返回 `(原任务, true)`。
    /// 查找与注册在同一把锁内，弱网并发重试不会创建第二个任务。
    pub fn create_with_idempotency(
        &self,
        owner_ns: &str,
        kind: TaskKind,
        total: u32,
        idempotency_key: Option<&str>,
    ) -> Result<(TaskView, bool), Problem> {
        self.create_inner(
            owner_ns,
            kind,
            TaskProgress::new(total, "source"),
            idempotency_key,
        )
    }

    fn create_inner(
        &self,
        owner_ns: &str,
        kind: TaskKind,
        progress: TaskProgress,
        idempotency_key: Option<&str>,
    ) -> Result<(TaskView, bool), Problem> {
        let now = now_ts();
        let mut inner = self.lock();
        prune(&mut inner, now, self.ttl_secs);
        if let Some(key) = idempotency_key {
            let map_key = (owner_ns.to_string(), key.to_string());
            if let Some(task_id) = inner.idempotency.get(&map_key).cloned() {
                if let Some(entry) = inner.tasks.get(&task_id) {
                    return Ok((entry.view.clone(), true));
                }
                inner.idempotency.remove(&map_key);
            }
        }
        self.check_active_capacity(&inner, owner_ns)?;

        // 注册表容量：先驱逐最老的已结束任务；全是活跃任务才拒绝
        while inner.tasks.len() >= REGISTRY_MAX_TASKS {
            if !evict_oldest_finished(&mut inner) {
                return Err(task_limit("任务注册表已满"));
            }
        }
        // 结果内存预算：同上；预算内没有可驱逐的已结束任务就拒绝新任务
        while inner.total_result_bytes >= self.global_result_budget {
            if !evict_oldest_finished(&mut inner) {
                return Err(task_limit("任务结果内存预算已满"));
            }
        }

        inner.next_seq += 1;
        let seq = inner.next_seq;
        let id = format!("tsk_{seq}");
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let view = TaskView {
            id: id.clone(),
            kind,
            state: TaskState::Queued,
            progress,
            created_at: rfc3339_secs(now),
            started_at: None,
            finished_at: None,
            result_href: None,
            error: None,
        };
        inner.tasks.insert(
            id.clone(),
            Entry {
                owner_ns: owner_ns.to_string(),
                seq,
                view: view.clone(),
                cancel: Arc::new(AtomicBool::new(false)),
                cancel_notify: Arc::new(Notify::new()),
                events: VecDeque::new(),
                event_bytes: 0,
                next_event_id: 0,
                tx,
                results: Vec::new(),
                dedupe_index: HashMap::new(),
                result_bytes: 0,
                truncated: false,
                finished_epoch: None,
            },
        );
        if let Some(key) = idempotency_key {
            inner
                .idempotency
                .insert((owner_ns.to_string(), key.to_string()), id);
        }
        Ok((view, false))
    }

    fn check_active_capacity(&self, inner: &Inner, owner_ns: &str) -> Result<(), Problem> {
        let active = inner
            .tasks
            .values()
            .filter(|entry| !entry.view.state.is_terminal())
            .count();
        let user_active = inner
            .tasks
            .values()
            .filter(|entry| entry.owner_ns == owner_ns && !entry.view.state.is_terminal())
            .count();
        if active >= self.global_limit || user_active >= self.per_user_limit {
            return Err(Problem::new(ErrorCode::TaskLimit)
                .detail(format!(
                    "同时运行的任务已达上限（每用户 {} 个、全局 {} 个）",
                    self.per_user_limit, self.global_limit
                ))
                .retry_after(TASK_LIMIT_RETRY_AFTER_SECS));
        }
        Ok(())
    }

    pub fn get(&self, owner_ns: &str, id: &str) -> Option<TaskView> {
        let mut inner = self.lock();
        prune(&mut inner, now_ts(), self.ttl_secs);
        inner
            .tasks
            .get(id)
            .filter(|entry| entry.owner_ns == owner_ns)
            .map(|entry| entry.view.clone())
    }

    /// 我的任务，按序号倒序（最新在前）；`state` 精确过滤。
    /// `cursor` 是上一页的 `nextCursor`（不透明），`limit` 由调用方校验。
    pub fn list(
        &self,
        owner_ns: &str,
        state: Option<TaskState>,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<TaskPage, Problem> {
        let cursor = match cursor.filter(|raw| !raw.trim().is_empty()) {
            Some(raw) => Some(decode_seq_cursor(raw)?),
            None => None,
        };
        let mut inner = self.lock();
        prune(&mut inner, now_ts(), self.ttl_secs);
        let mut entries: Vec<&Entry> = inner
            .tasks
            .values()
            .filter(|entry| entry.owner_ns == owner_ns)
            .filter(|entry| state.is_none_or(|want| entry.view.state == want))
            .collect();
        let total = entries.len() as u32;
        if let Some(cursor) = cursor {
            entries.retain(|entry| entry.seq < cursor);
        }
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.seq));
        let has_more = entries.len() > limit as usize;
        let items: Vec<TaskView> = entries
            .into_iter()
            .take(limit as usize)
            .map(|entry| entry.view.clone())
            .collect();
        let next_cursor = if has_more {
            items.last().map(|view| encode_seq_cursor(seq_of(view)))
        } else {
            None
        };
        Ok(TaskPage {
            items,
            next_cursor,
            total,
        })
    }

    /// 幂等预查：同一个 `Idempotency-Key` 返回原任务（用于参数解析前的快速重放）。
    pub fn find_idempotent(&self, owner_ns: &str, key: &str) -> Option<TaskView> {
        let mut inner = self.lock();
        prune(&mut inner, now_ts(), self.ttl_secs);
        let map_key = (owner_ns.to_string(), key.to_string());
        let task_id = inner.idempotency.get(&map_key)?.clone();
        let view = inner
            .tasks
            .get(&task_id)
            .filter(|entry| entry.owner_ns == owner_ns)
            .map(|entry| entry.view.clone());
        if view.is_none() {
            inner.idempotency.remove(&map_key);
        }
        view
    }

    /// queued → running；重复调用无副作用。
    pub fn mark_running(&self, id: &str) {
        let mut inner = self.lock();
        let capacity = self.event_capacity;
        let max_bytes = self.event_max_bytes;
        let Some(entry) = inner.tasks.get_mut(id) else {
            return;
        };
        if entry.view.state != TaskState::Queued {
            return;
        }
        let now = now_ts();
        entry.view.state = TaskState::Running;
        entry.view.started_at = Some(rfc3339_secs(now));
        let progress = entry.view.progress.clone();
        push_event(entry, capacity, max_bytes, "progress", &progress);
    }

    /// 更新进度并发 `progress` 事件。
    pub fn emit_progress(&self, id: &str, done: u32, total: u32, message: Option<String>) {
        let mut inner = self.lock();
        let capacity = self.event_capacity;
        let max_bytes = self.event_max_bytes;
        let Some(entry) = inner.tasks.get_mut(id) else {
            return;
        };
        if entry.view.state.is_terminal() {
            return;
        }
        entry.view.progress.done = done;
        entry.view.progress.total = total;
        entry.view.progress.message = message;
        let progress = entry.view.progress.clone();
        push_event(entry, capacity, max_bytes, "progress", &progress);
    }

    /// 追加一批搜索结果：按 `dedupeKey` 合并，返回新增与被合并更新的条目。
    /// `truncated` 后不再接收新条目（已有条目仍可补齐字段）。
    pub fn append_search_results(&self, id: &str, books: Vec<SearchBook>) -> Vec<SearchBook> {
        let mut inner = self.lock();
        let capacity = self.event_capacity;
        let max_bytes = self.event_max_bytes;
        let result_max = self.result_max_bytes;
        let budget = self.global_result_budget;
        let Inner {
            tasks,
            total_result_bytes,
            ..
        } = &mut *inner;
        let Some(entry) = tasks.get_mut(id) else {
            return Vec::new();
        };
        if entry.view.state.is_terminal() {
            return Vec::new();
        }
        let mut items = Vec::new();
        for mut book in books {
            let key = crate::service::search_merge::dedupe_key_for(&book);
            book.dedupe_key = Some(key.clone());
            if let Some(index) = entry.dedupe_index.get(&key).copied() {
                // 先在克隆上合并并量字节：补齐字段同样受限，否则小条目可以
                // 被后续书源补一个超大 intro 顶穿单结果集/全局预算
                let mut candidate = entry.results[index].clone();
                if !crate::service::search_merge::merge_into(&mut candidate, book) {
                    continue;
                }
                let before = serialized_len(&entry.results[index]);
                let after = serialized_len(&candidate);
                let delta = after.saturating_sub(before);
                if entry.result_bytes + delta > result_max || *total_result_bytes + delta > budget {
                    entry.truncated = true;
                    continue;
                }
                entry.result_bytes += delta;
                *total_result_bytes += delta;
                entry.results[index] = candidate.clone();
                items.push(candidate);
                continue;
            }
            if entry.truncated {
                break;
            }
            let size = serialized_len(&book);
            if entry.result_bytes + size > result_max || *total_result_bytes + size > budget {
                entry.truncated = true;
                break;
            }
            entry.result_bytes += size;
            *total_result_bytes += size;
            entry.dedupe_index.insert(key, entry.results.len());
            entry.results.push(book.clone());
            items.push(book);
        }
        if !items.is_empty() && entry.view.result_href.is_none() {
            entry.view.result_href = Some(format!("/v1/tasks/{id}/result"));
        }
        if !items.is_empty() {
            let cursor = encode_offset_for(entry.results.len(), entry.seq);
            emit_item_events(entry, &items, &cursor, capacity, max_bytes);
        }
        items
    }

    /// 任务结束：更新状态、`error` 并写 `done` 事件（失败先写 `error`）。
    ///
    /// 已经收到取消请求（`cancelling`）时，非 `cancelled` 的终态会被强制改成
    /// `cancelled`——取消语义优先，避免 DELETE 与完成同时发生时「取消变成功」。
    pub fn finish(&self, id: &str, state: TaskState, error: Option<Problem>) {
        debug_assert!(state.is_terminal(), "finish 只接受终态");
        let mut inner = self.lock();
        finish_locked(
            &mut inner,
            id,
            state,
            error,
            self.event_capacity,
            self.event_max_bytes,
        );
    }

    /// 取消 watchdog 兜底：5 秒后仍停在 `cancelling` 就强制落 `cancelled`。
    pub fn force_cancel_timeout(&self, id: &str) {
        let mut inner = self.lock();
        let is_cancelling = inner
            .tasks
            .get(id)
            .is_some_and(|entry| entry.view.state == TaskState::Cancelling);
        if is_cancelling {
            finish_locked(
                &mut inner,
                id,
                TaskState::Cancelled,
                None,
                self.event_capacity,
                self.event_max_bytes,
            );
        }
    }

    /// 请求取消：置位 + 唤醒工作器；已结束的任务直接返回（DELETE 恒 204）。
    /// 第二个返回值表示这次调用是否**新发起**了取消（重复 DELETE 不再挂 watchdog）。
    pub fn request_cancel(&self, owner_ns: &str, id: &str) -> Option<(TaskView, bool)> {
        let mut inner = self.lock();
        prune(&mut inner, now_ts(), self.ttl_secs);
        let entry = inner
            .tasks
            .get_mut(id)
            .filter(|entry| entry.owner_ns == owner_ns)?;
        let newly_requested =
            !entry.view.state.is_terminal() && entry.view.state != TaskState::Cancelling;
        if newly_requested {
            entry.view.state = TaskState::Cancelling;
            entry.cancel.store(true, Ordering::SeqCst);
            entry.cancel_notify.notify_one();
        }
        Some((entry.view.clone(), newly_requested))
    }

    /// 取消句柄：工作器在调度循环里 `select!` 等待它。
    pub fn cancellation(&self, id: &str) -> Option<(Arc<AtomicBool>, Arc<Notify>)> {
        let inner = self.lock();
        inner
            .tasks
            .get(id)
            .map(|entry| (entry.cancel.clone(), entry.cancel_notify.clone()))
    }

    /// 结果分页：`cursor` 是上一页的 `nextCursor`（不透明；缺省从 0 开始）。
    pub fn result_page(
        &self,
        owner_ns: &str,
        id: &str,
        cursor: Option<&str>,
        limit: u32,
    ) -> Option<Result<TaskResultPage, Problem>> {
        let mut inner = self.lock();
        prune(&mut inner, now_ts(), self.ttl_secs);
        let entry = inner
            .tasks
            .get(id)
            .filter(|entry| entry.owner_ns == owner_ns)?;
        let offset = match cursor.filter(|raw| !raw.trim().is_empty()) {
            Some(raw) => match decode_offset(raw, entry.seq) {
                Some(offset) => offset,
                None => {
                    return Some(Err(Problem::new(ErrorCode::InvalidCursor)
                        .detail("结果游标无法解码或不属于该任务")))
                }
            },
            None => 0,
        };
        let total = entry.results.len();
        let start = offset.min(total);
        let end = (start + limit as usize).min(total);
        let items = entry.results[start..end].to_vec();
        let next_cursor = (end < total).then(|| encode_offset_for(end, entry.seq));
        Some(Ok(TaskResultPage {
            items,
            next_cursor,
            total: total as u32,
            truncated: entry.truncated,
        }))
    }

    /// 建立事件订阅：先 `subscribe` 再读环形缓冲，避免两段之间漏事件；
    /// 重放仅含 id > `last_event_id` 的条目。
    pub fn subscribe(
        &self,
        owner_ns: &str,
        id: &str,
        last_event_id: u64,
    ) -> Result<TaskSubscription, Problem> {
        let mut inner = self.lock();
        prune(&mut inner, now_ts(), self.ttl_secs);
        let Some(entry) = inner
            .tasks
            .get_mut(id)
            .filter(|entry| entry.owner_ns == owner_ns)
        else {
            return Err(Problem::new(ErrorCode::TaskNotFound).detail("任务不存在或已过期"));
        };
        if entry.tx.receiver_count() >= MAX_SUBSCRIBERS_PER_TASK {
            return Err(Problem::new(ErrorCode::TaskLimit)
                .detail("该任务的事件流订阅者过多")
                .retry_after(TASK_LIMIT_RETRY_AFTER_SECS));
        }
        let receiver = entry.tx.subscribe();
        let oldest = entry.events.front().map(|event| event.id).unwrap_or(0);
        let gap = if oldest == 0 {
            // 环被大事件/容量清空：只要发过事件且客户端落后，就存在不可重放段
            entry.next_event_id > last_event_id
        } else {
            oldest > last_event_id.saturating_add(1)
        };
        let replay = entry
            .events
            .iter()
            .filter(|event| event.id > last_event_id)
            .cloned()
            .collect();
        Ok(TaskSubscription {
            replay,
            receiver,
            terminal: entry.view.state.is_terminal(),
            gap,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // 锁中毒时恢复内部状态继续服务：任务注册表不承载需要一致性的持久数据，
        // 让所有任务接口在单次 panic 后永久 500 才是更大的故障
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }
}

/// `item` 事件的数据体：新增/被合并更新的结果 + 结果集当前游标。
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskItemEvent {
    pub items: Vec<SearchBook>,
    /// 结果集当前游标（可交给 `GET /v1/tasks/{id}/result?cursor=`）
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskDoneEvent {
    pub state: TaskState,
    pub result_href: Option<String>,
}

fn task_limit(detail: impl Into<String>) -> Problem {
    Problem::new(ErrorCode::TaskLimit)
        .detail(detail)
        .retry_after(TASK_LIMIT_RETRY_AFTER_SECS)
}

fn finish_locked(
    inner: &mut Inner,
    id: &str,
    state: TaskState,
    error: Option<Problem>,
    capacity: usize,
    max_bytes: usize,
) {
    let Some(entry) = inner.tasks.get_mut(id) else {
        return;
    };
    if entry.view.state.is_terminal() {
        return;
    }
    let state = if entry.view.state == TaskState::Cancelling && state != TaskState::Cancelled {
        TaskState::Cancelled
    } else {
        state
    };
    let error = if state == TaskState::Failed {
        error
    } else {
        None
    };
    let now = now_ts();
    entry.view.state = state;
    entry.view.finished_at = Some(rfc3339_secs(now));
    entry.finished_epoch = Some(now);
    if let Some(problem) = error {
        entry.view.error = Some(problem.clone());
        push_event(entry, capacity, max_bytes, "error", &problem);
    }
    let done = TaskDoneEvent {
        state,
        result_href: entry.view.result_href.clone(),
    };
    push_event(entry, capacity, max_bytes, "done", &done);
}

/// `item` 事件单块目标大小：超过就拆成多条事件，保证每条都能留在事件环里
/// 被 `Last-Event-ID` 重放（单本超大结果不进 SSE，客户端走 result 分页）。
const ITEM_EVENT_CHUNK_BYTES: usize = 32 * 1024;
/// broadcast 通道容量：实时订阅者消费很快；慢订阅者收到 Lagged 后重连，
/// 由事件环补齐。固定小值避免每个任务囤积数百条大事件。
const BROADCAST_CAPACITY: usize = 32;

fn emit_item_events(
    entry: &mut Entry,
    items: &[SearchBook],
    cursor: &str,
    capacity: usize,
    max_bytes: usize,
) {
    let mut chunk: Vec<SearchBook> = Vec::new();
    let mut chunk_bytes = 0usize;
    let flush = |chunk: &mut Vec<SearchBook>, entry: &mut Entry| {
        if chunk.is_empty() {
            return;
        }
        let payload = TaskItemEvent {
            items: std::mem::take(chunk),
            cursor: Some(cursor.to_string()),
        };
        push_event(entry, capacity, max_bytes, "item", &payload);
    };
    for item in items {
        let size = serialized_len(item);
        if size > ITEM_EVENT_CHUNK_BYTES {
            // 单条超过事件块预算：不发 SSE（事件环放不下），结果仍可经
            // `GET /v1/tasks/{id}/result` 分页取到
            tracing::warn!(
                task_id = %entry.view.id,
                size,
                "单条搜索结果超过事件块预算，跳过 item 事件"
            );
            continue;
        }
        if !chunk.is_empty() && chunk_bytes + size > ITEM_EVENT_CHUNK_BYTES {
            flush(&mut chunk, entry);
            chunk_bytes = 0;
        }
        chunk_bytes += size;
        chunk.push(item.clone());
    }
    flush(&mut chunk, entry);
}

fn push_event<T: Serialize>(
    entry: &mut Entry,
    capacity: usize,
    max_bytes: usize,
    name: &str,
    payload: &T,
) {
    let data = match serde_json::to_string(payload) {
        Ok(data) => data,
        Err(error) => {
            // 理论上不会发生（所有 payload 都是普通结构体）；真发生时给一个
            // problem 形态的 data，绝不让事件变成 "{}" 这类无法解释的形状
            tracing::error!(%error, event = name, "任务事件序列化失败");
            r#"{"code":"INTERNAL","status":500,"title":"Internal Server Error"}"#.to_string()
        }
    };
    entry.next_event_id += 1;
    let event = TaskEvent {
        id: entry.next_event_id,
        event: name.to_string(),
        data,
    };
    entry.event_bytes += event.data.len() + event.event.len() + 16;
    entry.events.push_back(event.clone());
    while entry.events.len() > capacity || entry.event_bytes > max_bytes {
        let Some(evicted) = entry.events.pop_front() else {
            break;
        };
        entry.event_bytes = entry
            .event_bytes
            .saturating_sub(evicted.data.len() + evicted.event.len() + 16);
    }
    let _ = entry.tx.send(event);
}

fn serialized_len(book: &SearchBook) -> usize {
    serde_json::to_vec(book)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

fn random_sequence_base() -> u64 {
    // 随机基数 + 递增：进程重启后旧 id 不会指向新任务（碰撞概率可忽略）
    let random = rand::random::<u64>() % 1_000_000_000_000;
    random + 1
}

fn seq_of(view: &TaskView) -> u64 {
    view.id
        .strip_prefix("tsk_")
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(0)
}

fn encode_offset_for(offset: usize, task_seq: u64) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&serde_json::json!({ "o": offset, "t": task_seq }))
            .expect("cursor json"),
    )
}

fn decode_offset(raw: &str, task_seq: u64) -> Option<usize> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw)
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let bound_task = value.get("t")?.as_u64()?;
    if bound_task != task_seq {
        return None;
    }
    value.get("o")?.as_u64().map(|offset| offset as usize)
}

fn encode_seq_cursor(seq: u64) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&serde_json::json!({ "s": seq })).expect("cursor json"))
}

fn decode_seq_cursor(raw: &str) -> Result<u64, Problem> {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value.get("s")?.as_u64())
        .ok_or_else(|| Problem::new(ErrorCode::InvalidCursor).detail("任务游标无法解码"))
}

fn prune(inner: &mut Inner, now: i64, ttl_secs: i64) {
    let Inner {
        tasks,
        total_result_bytes,
        idempotency,
        ..
    } = inner;
    tasks.retain(|_, entry| {
        let keep = entry
            .finished_epoch
            .is_none_or(|finished| finished + ttl_secs > now);
        if !keep {
            *total_result_bytes = total_result_bytes.saturating_sub(entry.result_bytes);
        }
        keep
    });
    idempotency.retain(|_, task_id| tasks.contains_key(task_id));
}

/// 注册表/预算压力下驱逐最早结束的任务；全是活跃任务则返回 false。
fn evict_oldest_finished(inner: &mut Inner) -> bool {
    let oldest = inner
        .tasks
        .iter()
        .filter_map(|(id, entry)| entry.finished_epoch.map(|epoch| (id.clone(), epoch)))
        .min_by_key(|(_, epoch)| *epoch);
    if let Some((id, _)) = oldest {
        if let Some(entry) = inner.tasks.remove(&id) {
            inner.total_result_bytes = inner.total_result_bytes.saturating_sub(entry.result_bytes);
        }
        inner.idempotency.retain(|_, task_id| *task_id != id);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(name: &str, author: &str, source_id: i64) -> SearchBook {
        SearchBook {
            name: name.into(),
            author: author.into(),
            book_url: format!("https://{source_id}.example/{name}"),
            origin: format!("https://{source_id}.example"),
            book_source_id: Some(source_id),
            ..SearchBook::default()
        }
    }

    #[test]
    fn create_enforces_per_user_and_global_limits() {
        let service = TaskService::with_limits(8, 2, 16, 1024, 3600);
        service.create("user_1", TaskKind::Search, 1).unwrap();
        service.create("user_1", TaskKind::Search, 1).unwrap();
        let limit = service
            .create("user_1", TaskKind::Search, 1)
            .expect_err("第三个任务应超限");
        assert_eq!(limit.code(), ErrorCode::TaskLimit);
        // 另一个用户不受影响（全局还有空位）
        service.create("user_2", TaskKind::Search, 1).unwrap();
    }

    #[test]
    fn append_dedupes_by_normalized_key_and_marks_truncated() {
        let service = TaskService::with_limits(8, 4, 32, 400, 3600);
        let task = service.create("user_1", TaskKind::Search, 2).unwrap();
        let added = service.append_search_results(
            &task.id,
            vec![book("三体", "刘慈欣", 1), book("三體（全本）", "劉慈欣", 2)],
        );
        // 第二条同 key：一次 append 里既是「新增」也是「合并更新」，两条都进 item 事件
        assert_eq!(added.len(), 2);
        assert!(added[0].dedupe_key.is_some());

        // 同 key 的第二条是「更新」而不是新增，且能补齐字段
        let updated = service.append_search_results(
            &task.id,
            vec![SearchBook {
                intro: Some("简介".into()),
                ..book("三体", "刘慈欣", 2)
            }],
        );
        assert_eq!(updated.len(), 1, "合并更新也进 item 事件");
        let page = service
            .result_page("user_1", &task.id, None, 100)
            .unwrap()
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].intro.as_deref(), Some("简介"));
        assert_eq!(page.items[0].book_source_ids, Some(vec![1, 2]));

        // 400 字节上限：继续塞书会截断
        for index in 0..50 {
            service.append_search_results(
                &task.id,
                vec![book(&format!("很长的书名{index}"), "作者", 3)],
            );
        }
        let page = service
            .result_page("user_1", &task.id, None, 100)
            .unwrap()
            .unwrap();
        assert!(page.truncated, "结果集应标记 truncated");
        assert!(page.total < 51);
    }

    #[test]
    fn event_ring_replays_after_last_event_id() {
        let service = TaskService::with_limits(8, 4, 3, 10_000, 3600);
        let task = service.create("user_1", TaskKind::Search, 1).unwrap();
        service.mark_running(&task.id);
        service.emit_progress(&task.id, 1, 1, Some("done".into()));
        service.finish(&task.id, TaskState::Succeeded, None);

        let all = service.subscribe("user_1", &task.id, 0).unwrap();
        assert_eq!(all.replay.len(), 3, "progress/progress/done");
        assert!(all.terminal);

        let tail = service
            .subscribe("user_1", &task.id, all.replay[1].id)
            .unwrap();
        assert_eq!(tail.replay.len(), 1, "只重放 done");
        assert_eq!(tail.replay[0].event, "done");

        // 已经收到 done 的重连：没有新事件，replay 为空
        let end = service
            .subscribe("user_1", &task.id, all.replay.last().unwrap().id)
            .unwrap();
        assert!(end.replay.is_empty());
    }

    #[test]
    fn finish_respects_cancel_request() {
        let service = TaskService::with_limits(8, 4, 16, 10_000, 3600);
        let task = service.create("user_1", TaskKind::Search, 1).unwrap();
        service.mark_running(&task.id);
        service.request_cancel("user_1", &task.id).unwrap();
        // DELETE 与「自然完成」同时发生：取消优先
        service.finish(&task.id, TaskState::Succeeded, None);
        assert_eq!(
            service.get("user_1", &task.id).unwrap().state,
            TaskState::Cancelled
        );
    }

    #[test]
    fn list_sorts_numerically_and_paginates() {
        let service = TaskService::with_limits(64, 64, 8, 1024, 3600);
        for _ in 0..12 {
            let task = service.create("user_1", TaskKind::Search, 1).unwrap();
            service.finish(&task.id, TaskState::Succeeded, None);
        }
        let first = service.list("user_1", None, None, 5).unwrap();
        assert_eq!(first.items.len(), 5);
        assert_eq!(first.total, 12);
        assert!(first.next_cursor.is_some());
        let first_ids: Vec<u64> = first.items.iter().map(seq_of).collect();
        assert!(
            first_ids.windows(2).all(|pair| pair[0] > pair[1]),
            "应按序号倒序: {first_ids:?}"
        );

        let second = service
            .list("user_1", None, first.next_cursor.as_deref(), 5)
            .unwrap();
        assert_eq!(second.items.len(), 5);
        assert!(second
            .items
            .iter()
            .all(|view| !first_ids.contains(&seq_of(view))));

        let third = service
            .list("user_1", None, second.next_cursor.as_deref(), 5)
            .unwrap();
        assert_eq!(third.items.len(), 2);
        assert_eq!(third.next_cursor, None);
    }

    #[test]
    fn merge_respects_result_budget() {
        // 单结果集预算 400 字节：小条目先落库，后续书源补超大 intro 时
        // 不能把结果顶穿预算，只能标 truncated
        let service = TaskService::with_limits(8, 4, 32, 400, 3600);
        let task = service.create("user_1", TaskKind::Search, 2).unwrap();
        service.append_search_results(&task.id, vec![book("三体", "刘慈欣", 1)]);
        let merged = service.append_search_results(
            &task.id,
            vec![SearchBook {
                intro: Some("超".repeat(1000)),
                ..book("三体", "刘慈欣", 2)
            }],
        );
        assert!(merged.is_empty(), "超预算的合并应被拒绝: {merged:?}");
        let page = service
            .result_page("user_1", &task.id, None, 10)
            .unwrap()
            .unwrap();
        assert!(page.truncated);
        assert!(page.items[0].intro.is_none());
        assert_eq!(page.items[0].book_source_ids, None);
    }

    #[test]
    fn large_item_batches_are_chunked_into_replayable_events() {
        let service = TaskService::with_limits(8, 4, 64, 10_000_000, 3600);
        let task = service.create("user_1", TaskKind::Search, 1).unwrap();
        service.mark_running(&task.id);
        let books: Vec<SearchBook> = (0..8)
            .map(|index| SearchBook {
                intro: Some("长".repeat(10_000)),
                ..book(&format!("书{index}"), "作者", 1)
            })
            .collect();
        service.append_search_results(&task.id, books);

        let subscription = service.subscribe("user_1", &task.id, 0).unwrap();
        let item_events: Vec<_> = subscription
            .replay
            .iter()
            .filter(|event| event.event == "item")
            .collect();
        assert!(item_events.len() > 1, "大 batch 应拆成多条 item 事件");
        assert!(
            item_events
                .iter()
                .all(|event| event.data.len() <= ITEM_EVENT_CHUNK_BYTES + 1024),
            "单条 item 事件不应超过块预算"
        );
        // 环没有被超大事件清空：最早事件仍可重放
        assert!(!subscription.gap);
    }
}
