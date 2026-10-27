//! AI 资料编排 sidecar 服务。
//!
//! 每个任务 spawn 一个 Python sidecar 进程（stdio NDJSON 协议，见
//! `sidecar/agent_sidecar/protocol.py`），批量章节循环在 Rust 侧：
//! 逐章发送 run 帧 → 等待 result 帧 → 落库 → 下一章。sidecar 无状态，
//! memory 随每帧全量下发；章节正文经工具回调（tool_call/tool_result）从
//! 统一内容链路获取，覆盖本地 TXT/EPUB 与在线书源。
//!
//! 关键约定：
//! - stdout 只读 NDJSON；非 JSON 行记 warn 跳过（不杀进程）；
//! - stderr 持续排空进 tracing（不排空会因管道背压把 sidecar 卡死）；
//! - 单章处理超时（`AGENT_SIDECAR_CHAPTER_TIMEOUT_SECS`，默认 1800s，与
//!   sidecar 内模型调用总预算对齐——预算默认 1500s=5×300s，经
//!   `AGENT_SIDECAR_MODEL_BUDGET_SECS` 下发，算式见 config.rs）即杀进程
//!   中止批量；
//! - 取消 = kill 进程；sidecar 在 stdin EOF 时自行退出，不留孤儿。

use std::{
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};

use base64::Engine as _;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{mpsc, Mutex as AsyncMutex},
    time::timeout,
};

use crate::{
    error::error::AppError,
    model::{
        ai_book::{AiBookMap, AiBookMemory},
        ai_model::{AiModelKind, ResolvedAiModelEndpoint},
        book::Book,
        book_chapter::BookChapter,
        book_source::BookSource,
    },
    service::{
        ai_book_service::AiBookService,
        ai_model_service::AiModelService,
        book_service::BookService,
        book_source_service::BookSourceService,
        local_epub_book::{is_local_epub_origin, is_local_epub_url, LocalEpubBookService},
        local_txt_book::{is_local_txt_origin, is_local_txt_url, LocalTxtBookService},
    },
    util::{asset::write_asset_file, hash::md5_hex, text::repair_encoded_url, time::now_ts},
};

/// 协议版本（与 sidecar/agent_sidecar/__init__.py 的 PROTOCOL_VERSION 一致，
/// 握手处相等校验）：v2 起 tool_call/tool_result 帧携带 jobId 做归属校验
const PROTOCOL_VERSION: u32 = 2;
/// 与 config.rs 的默认值保持一致：仅当命令未被自定义时才应用 venv 优先逻辑
const DEFAULT_SIDECAR_COMMAND: &str = "python -m agent_sidecar";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// uv 模式下首次 `uv run` 可能现场同步依赖（下载/建 venv），握手超时放宽
const HANDSHAKE_TIMEOUT_UV: Duration = Duration::from_secs(60);
/// sidecar stdout 单行（NDJSON 帧）上限：与 Python 侧 protocol.py 的
/// MAX_LINE_BYTES 一致。长篇书的 memory 行可达几十 MB，64MB 已极端宽裕；
/// 超限判定对端失控，报错并中止任务而不是把内存推高
const MAX_NDJSON_LINE_BYTES: usize = 64 * 1024 * 1024;

/// sidecar 子进程允许继承的环境变量名（白名单，见 `sidecar_env_allowlist`）。
///
/// sidecar 只需要「找到 Python/uv、解析 TLS 证书、走系统代理」；宿主进程里的
/// `JWT_SECRET` / `ADMIN_PASSWORD` / `AI_*` 的 API Key / `DATABASE_URL` 等
/// 服务端凭据一律不进子进程——失控的 sidecar（或其依赖链）不该有机会读到它们。
/// `UV_` 前缀（uv 自己的配置）与 `AGENT_SIDECAR_` 前缀（sidecar 自己的日志级别
/// 等开关）整组放行；Windows 变量名大小写不敏感（`PATH` 可能存成 `Path`），
/// 匹配时忽略大小写。
const SIDECAR_ENV_KEYS: &[&str] = &[
    // 进程与 shell 解析（Windows 缺 SystemRoot 连 Python 都起不来）
    "PATH",
    "PATHEXT",
    "SystemRoot",
    "SystemDrive",
    "ComSpec",
    // 临时目录（pip/uv 落盘要用；TMPDIR 是 Unix 侧的约定名）
    "TEMP",
    "TMP",
    "TMPDIR",
    // 用户目录与漫游数据（uv 缓存默认在 %LOCALAPPDATA%\uv）
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    // 区域与编码
    "LANG",
    "LC_ALL",
    "PYTHONIOENCODING",
    "PYTHONUTF8",
    // TLS 证书与系统代理（大小写两种都放行；ALL_PROXY 是 httpx/curl
    // 认的通用代理变量，SOCKS 代理只走它）
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

fn is_allowed_env_key(name: &str) -> bool {
    if name.starts_with("UV_") || name.starts_with("AGENT_SIDECAR_") {
        return true;
    }
    if SIDECAR_ENV_KEYS.contains(&name) {
        return true;
    }
    #[cfg(windows)]
    {
        SIDECAR_ENV_KEYS
            .iter()
            .any(|key| key.eq_ignore_ascii_case(name))
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 从父进程环境挑出 sidecar 允许继承的变量（纯函数，便于单测）。
#[cfg(test)]
fn sidecar_env_allowlist_from<I, K, V>(vars: I) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    vars.into_iter()
        .filter(|(name, _)| is_allowed_env_key(name.as_ref()))
        .map(|(name, value)| (name.as_ref().to_string(), value.as_ref().to_string()))
        .collect()
}

/// spawn 时实际使用的白名单：父进程变量过滤 + 两个编码兜底。
///
/// `PYTHONIOENCODING` / `PYTHONUTF8` 未显式配置时强制 utf-8：sidecar 的
/// stdio NDJSON 是 UTF-8，Windows 上 Python 对管道默认用 ANSI 代码页，
/// 不兜底的话中文帧会整体变成非法 UTF-8 而被逐行丢弃。
fn sidecar_env_allowlist() -> Vec<(String, String)> {
    let mut vars: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(name, value)| Some((name.to_str()?.to_string(), value.to_str()?.to_string())))
        .filter(|(name, _)| is_allowed_env_key(name))
        .collect();
    fn ensure_set(vars: &mut Vec<(String, String)>, key: &str, value: &str) {
        let present = vars.iter().any(|(name, _)| name.eq_ignore_ascii_case(key));
        if !present {
            vars.push((key.to_string(), value.to_string()));
        }
    }
    ensure_set(&mut vars, "PYTHONIOENCODING", "utf-8");
    ensure_set(&mut vars, "PYTHONUTF8", "1");
    vars
}

/// sidecar 启动计划：四级优先级（详见 resolve_spawn_plan）。
#[derive(Debug, Clone, PartialEq)]
enum SpawnPlan {
    /// 显式自定义命令（AGENT_SIDECAR_COMMAND 非默认；测试与自托管逃生门）
    Custom(String, Vec<String>),
    /// uv 自动管理依赖（uv run --project <dir> python -m agent_sidecar）
    Uv(String),
    /// 降级：默认命令直接跑（需宿主机已装好 sidecar 依赖）
    Default(String, Vec<String>),
}
const SIDECAR_LOG_TARGET: &str = "agent_sidecar";
const MAP_IMAGE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_MAP_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// 章节窗口：index ≤ 30 时下发 `[0, index+21]`——「最新章前置」识别只在
/// 前 31 章生效，且需要从第 0 章找第一个正文章；其余下发 ±10 仅供 TOC 附加。
const CHAPTER_WINDOW_NEAR_START: i32 = 30;
/// 单次章节搜索最多扫描的章数（有内容缓存兜底）。
const SEARCH_MAX_CHAPTERS: usize = 40;
const SEARCH_MAX_SNIPPETS: usize = 5;
const SEARCH_SNIPPET_CHARS: usize = 240;
const SEARCH_CONTEXT_BEFORE: usize = 80;

const STDERR_TAIL_LINES: usize = 8;
const STDERR_LINE_CHARS: usize = 200;

type StderrTail = Arc<AsyncMutex<Vec<String>>>;

const PROBE_UNKNOWN: u8 = 0;
const PROBE_READY: u8 = 1;
const PROBE_FAILED: u8 = 2;

/// 任务状态快照。注册表持久保留最后一条（含 lastError），直到下一个任务
/// 或进程重启——关页不丢进度，重开页面轮询即恢复。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTaskSnapshot {
    pub running: bool,
    pub job_id: String,
    pub book_id: String,
    pub phase: String,
    pub status_text: String,
    pub current_chapter_index: Option<i32>,
    pub target_chapter_index: Option<i32>,
    pub last_error: Option<String>,
    /// 任务归属的用户命名空间：status/cancel 据此隔离，不下发到客户端。
    /// 单用户部署下恒等于唯一账号，这层是为多用户部署形态留的防线
    #[serde(skip)]
    pub owner_ns: String,
}

impl AgentTaskSnapshot {
    fn empty() -> Self {
        Self {
            running: false,
            job_id: String::new(),
            book_id: String::new(),
            phase: "idle".to_string(),
            status_text: String::new(),
            current_chapter_index: None,
            target_chapter_index: None,
            last_error: None,
            owner_ns: String::new(),
        }
    }
}

struct ActiveTask {
    cancel: Arc<AtomicBool>,
    child: Arc<AsyncMutex<Option<Child>>>,
    owner_ns: String,
}

/// 任务类型。
#[derive(Debug, Clone, Copy)]
pub enum AgentTaskKind {
    UpdateToCurrent,
    RedrawMap,
}

impl AgentTaskKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "update_to_current" => Some(Self::UpdateToCurrent),
            "redraw_map" => Some(Self::RedrawMap),
            _ => None,
        }
    }
}

/// 单章帧泵期间的工具上下文。
struct TaskContext<'a> {
    user_ns: &'a str,
    book: &'a Book,
    chapters: &'a [BookChapter],
    memory: &'a AiBookMemory,
}

#[derive(Clone)]
pub struct AgentSidecarService {
    book_service: Arc<BookService>,
    book_source_service: Arc<BookSourceService>,
    local_txt: Arc<LocalTxtBookService>,
    local_epub: Arc<LocalEpubBookService>,
    ai_book_service: Arc<AiBookService>,
    ai_model_service: Arc<AiModelService>,
    command: String,
    uv_path: String,
    enabled: bool,
    chapter_timeout: Duration,
    /// sidecar 内模型调用总预算（秒）：spawn 时经 AGENT_SIDECAR_MODEL_BUDGET_SECS 下发
    model_budget_secs: u64,
    sidecar_dir: PathBuf,
    assets_dir: String,
    registry: Arc<AsyncMutex<Option<AgentTaskSnapshot>>>,
    active: Arc<AsyncMutex<Option<Arc<ActiveTask>>>>,
    probe: Arc<AtomicU8>,
    /// PATH 探测结果缓存（probe 第一次 spawn 时解析，之后复用）
    spawn_plan: Arc<AsyncMutex<Option<SpawnPlan>>>,
}

impl AgentSidecarService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        book_service: Arc<BookService>,
        book_source_service: Arc<BookSourceService>,
        local_txt: Arc<LocalTxtBookService>,
        local_epub: Arc<LocalEpubBookService>,
        ai_book_service: Arc<AiBookService>,
        ai_model_service: Arc<AiModelService>,
        command: &str,
        uv_path: &str,
        enabled: bool,
        chapter_timeout_secs: u64,
        model_budget_secs: u64,
        assets_dir: &str,
    ) -> Self {
        Self {
            book_service,
            book_source_service,
            local_txt,
            local_epub,
            ai_book_service,
            ai_model_service,
            command: command.to_string(),
            uv_path: uv_path.to_string(),
            enabled,
            chapter_timeout: Duration::from_secs(chapter_timeout_secs.max(1)),
            model_budget_secs: model_budget_secs.max(1),
            sidecar_dir: resolve_sidecar_dir(),
            assets_dir: assets_dir.to_string(),
            registry: Arc::new(AsyncMutex::new(None)),
            active: Arc::new(AsyncMutex::new(None)),
            probe: Arc::new(AtomicU8::new(PROBE_UNKNOWN)),
            spawn_plan: Arc::new(AsyncMutex::new(None)),
        }
    }

    /// 解析启动计划（惰性 + 缓存）：PATH 探测只在首次 spawn 时做一次。
    async fn spawn_plan(&self) -> SpawnPlan {
        let mut guard = self.spawn_plan.lock().await;
        if let Some(plan) = guard.clone() {
            return plan;
        }
        let plan = resolve_spawn_plan(&self.command, &self.uv_path, uv_in_path().await);
        *guard = Some(plan.clone());
        plan
    }

    /// sidecar 可用性（启动时握手探测的结果）。
    pub fn agent_ready(&self) -> bool {
        self.enabled && self.probe.load(Ordering::Relaxed) == PROBE_READY
    }

    /// 启动探测：spawn 一次并完成握手即认为可用；失败只记 warn，不影响主服务。
    pub async fn probe(self: Arc<Self>) {
        if !self.enabled {
            self.probe.store(PROBE_FAILED, Ordering::Relaxed);
            return;
        }
        match self.spawn_sidecar().await {
            Ok((mut child, mut stdin, _rx, _tail)) => {
                let _ = stdin.shutdown().await;
                let _ = child.start_kill();
                self.probe.store(PROBE_READY, Ordering::Relaxed);
                tracing::info!(target: SIDECAR_LOG_TARGET, "sidecar 握手探测通过");
            }
            Err(error) => {
                self.probe.store(PROBE_FAILED, Ordering::Relaxed);
                tracing::warn!(
                    target: SIDECAR_LOG_TARGET,
                    "sidecar 握手探测失败，AI 资料功能不可用：{error:#}"
                );
            }
        }
    }

    /// 提交任务。已有任务进行中返回 409。
    pub async fn start_task(
        &self,
        user_ns: &str,
        book: Book,
        kind: AgentTaskKind,
        target_chapter_index: Option<i32>,
    ) -> Result<String, AppError> {
        if !self.enabled {
            return Err(AppError::BadRequest(
                "AI 资料编排未启用（AGENT_SIDECAR_ENABLED=false）".to_string(),
            ));
        }
        // 检查与写入必须在同一临界区完成，否则两个并发请求可双双通过检查
        let book_id = book.book_id.clone().unwrap_or_default();
        let job_id = format!("agent-{}-{}", now_ts(), book_id);
        let task = Arc::new(ActiveTask {
            cancel: Arc::new(AtomicBool::new(false)),
            child: Arc::new(AsyncMutex::new(None)),
            owner_ns: user_ns.to_string(),
        });
        {
            let mut active = self.active.lock().await;
            if active.is_some() {
                return Err(AppError::Conflict("已有任务进行中".to_string()));
            }
            *active = Some(task.clone());
        }
        {
            let mut registry = self.registry.lock().await;
            *registry = Some(AgentTaskSnapshot {
                running: true,
                job_id: job_id.clone(),
                book_id,
                phase: "loading".to_string(),
                status_text: "准备任务...".to_string(),
                current_chapter_index: None,
                target_chapter_index,
                last_error: None,
                owner_ns: user_ns.to_string(),
            });
        }

        let service = self.clone();
        let user_ns = user_ns.to_string();
        let job_id_for_task = job_id.clone();
        tokio::spawn(async move {
            let (status_text, last_error) = match kind {
                AgentTaskKind::UpdateToCurrent => {
                    service
                        .run_update_task(
                            &user_ns,
                            &book,
                            target_chapter_index,
                            &job_id_for_task,
                            &task,
                        )
                        .await
                }
                AgentTaskKind::RedrawMap => {
                    service
                        .run_redraw_task(&user_ns, &book, &job_id_for_task, &task)
                        .await
                }
            };
            service
                .update_snapshot(|snapshot| {
                    snapshot.running = false;
                    snapshot.phase = if last_error.is_some() {
                        "error"
                    } else {
                        "idle"
                    }
                    .to_string();
                    snapshot.status_text = status_text;
                    snapshot.last_error = last_error;
                })
                .await;
            // 清理进程（取消路径已经 kill 过；这里兜底）
            if let Some(mut child) = task.child.lock().await.take() {
                let _ = child.kill().await;
            }
            let mut active = service.active.lock().await;
            if active
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &task))
            {
                *active = None;
            }
        });
        Ok(job_id)
    }

    /// 状态快照只对任务归属者可见：其他用户的视角永远是 idle
    /// （单用户部署下恒为同一人，这层为多用户形态兜底）
    pub async fn status(&self, user_ns: &str) -> AgentTaskSnapshot {
        self.registry
            .lock()
            .await
            .clone()
            .filter(|snapshot| snapshot.owner_ns == user_ns)
            .unwrap_or_else(AgentTaskSnapshot::empty)
    }

    /// 取消只对任务归属者生效；他人调用视为无任务可取消
    pub async fn cancel_task(&self, user_ns: &str) -> bool {
        let active = self.active.lock().await.clone();
        if let Some(task) = active {
            if task.owner_ns != user_ns {
                return false;
            }
            task.cancel.store(true, Ordering::SeqCst);
            if let Some(mut child) = task.child.lock().await.take() {
                let _ = child.kill().await;
            }
            return true;
        }
        false
    }
}

impl AgentSidecarService {
    async fn run_update_task(
        &self,
        user_ns: &str,
        book: &Book,
        target_override: Option<i32>,
        job_id: &str,
        task: &ActiveTask,
    ) -> (String, Option<String>) {
        match self
            .execute_update_task(user_ns, book, target_override, job_id, task)
            .await
        {
            Ok(status_text) => (status_text, None),
            Err(error) => {
                let text = task_error_text(&error);
                (text.clone(), Some(text))
            }
        }
    }

    async fn run_redraw_task(
        &self,
        user_ns: &str,
        book: &Book,
        job_id: &str,
        task: &ActiveTask,
    ) -> (String, Option<String>) {
        match self.execute_redraw_task(user_ns, book, job_id, task).await {
            Ok(status_text) => (status_text, None),
            Err(error) => {
                let text = task_error_text(&error);
                (text.clone(), Some(text))
            }
        }
    }

    async fn execute_update_task(
        &self,
        user_ns: &str,
        book: &Book,
        target_override: Option<i32>,
        job_id: &str,
        task: &ActiveTask,
    ) -> Result<String, AppError> {
        let mut memory = self
            .ai_book_service
            .get(user_ns, book.book_id.as_deref().unwrap_or(""))
            .await?
            .unwrap_or_else(|| default_memory(book));

        let chapters = self.load_chapter_list(user_ns, book).await?;
        if chapters.is_empty() {
            return Err(AppError::BadRequest("目录为空，无法更新".to_string()));
        }
        let max_index = (chapters.len() as i32) - 1;
        let target = target_override
            .or(book.dur_chapter_index)
            .unwrap_or(max_index)
            .clamp(0, max_index);
        // 上界 max_index + 1：processed 已是末章时 start 越过 target 走短路，
        // 而不是被钳回末章白跑一次模型
        let start = memory
            .processed_chapter_index
            .map(|index| index + 1)
            .unwrap_or(0)
            .clamp(0, max_index + 1);
        self.update_snapshot(|snapshot| {
            snapshot.target_chapter_index = Some(target);
        })
        .await;
        if start > target {
            return Ok("当前进度已更新".to_string());
        }

        // 模型就绪检查提前到 spawn 之前，快速失败
        let model = self.ai_model_service.get().await?;
        let text_endpoint = model.resolve(AiModelKind::Text);
        let image_endpoint = model.resolve(AiModelKind::Image);
        if !text_endpoint.enabled
            || text_endpoint.base_url.trim().is_empty()
            || text_endpoint.model.trim().is_empty()
        {
            return Err(AppError::BadRequest("文本模型未配置".to_string()));
        }
        let model_json = model_payload_json(&text_endpoint, &image_endpoint);

        let (child, mut stdin, mut rx, stderr_tail) = self.spawn_sidecar().await?;
        *task.child.lock().await = Some(child);

        let outcome: Result<String, AppError> = async {
            for index in start..=target {
                if task.cancel.load(Ordering::SeqCst) {
                    return Ok("已取消".to_string());
                }
                let chapter = &chapters[index as usize];
                self.update_snapshot(|snapshot| {
                    snapshot.current_chapter_index = Some(index);
                    snapshot.phase = "text".to_string();
                    snapshot.status_text = format!("更新 {} 的 AI 资料...", chapter.title);
                })
                .await;

                let run_frame = json!({
                    "type": "run",
                    "job": {
                        "jobId": job_id,
                        "kind": "chapter_update",
                        "book": {"name": book.name, "author": book.author, "bookUrl": book.book_url},
                        "chapter": {"index": index, "title": chapter.title, "url": chapter.url},
                        "chapters": chapters_window(&chapters, index),
                        "memory": serde_json::to_value(&memory).unwrap_or_default(),
                        "model": model_json,
                    }
                });
                let ctx = TaskContext {
                    user_ns,
                    book,
                    chapters: &chapters,
                    memory: &memory,
                };
                let result = self
                    .run_frame_until_result(&mut stdin, &mut rx, run_frame, job_id, &ctx, &stderr_tail)
                    .await?;

                if result.get("skipped").and_then(Value::as_bool) == Some(true) {
                    memory.processed_chapter_index = Some(index);
                    memory.processed_chapter_title = Some(chapter.title.clone());
                    memory.updated_at = now_ts() * 1000;
                    memory.last_error = None;
                    self.persist(user_ns, &memory).await?;
                    continue;
                }
                // sidecar 的真实失败（模型 401/超时等）必须原样上抛，不能落成「缺 memory」
                if result.get("ok").and_then(Value::as_bool) != Some(true) {
                    let error = result
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("AI 资料生成失败");
                    return Err(AppError::BadRequest(error.to_string()));
                }

                let memory_value = result
                    .get("memory")
                    .cloned()
                    .ok_or_else(|| AppError::Internal(anyhow::anyhow!("result 帧缺少 memory")))?;
                memory = serde_json::from_value(memory_value)
                    .map_err(|e| AppError::BadRequest(format!("sidecar 返回的资料无法解析：{e}")))?;
                // 身份字段以任务书籍为准（sidecar 回传的 JSON 可能不含 bookId）
                memory.book_id = book.book_id.clone();
                memory.book_url = book.book_url.clone();
                memory.updated_at = now_ts() * 1000;
                memory.last_error = None;

                // 地图按需重绘（失败降级关系图，不中断批量）
                let map_prompt = result
                    .get("mapPrompt")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .filter(|p| !p.trim().is_empty());
                let should_redraw =
                    result.get("shouldRegenerateMap").and_then(Value::as_bool) == Some(true)
                        && map_prompt.is_some();
                if should_redraw {
                    self.update_snapshot(|snapshot| {
                        snapshot.phase = "map".to_string();
                        snapshot.status_text = "生成世界地图...".to_string();
                    })
                    .await;
                    // redraw 帧的 jobId 必须与任务/章节绑定：超时降级后迟到的
                    // result 靠它被下一章的帧泵识别并跳过（串帧防线）
                    let redraw_job_id = format!("redraw-{job_id}-{index}");
                    match self.redraw_map_via_sidecar(&mut stdin, &mut rx, &memory, &model_json, &stderr_tail, &redraw_job_id).await {
                        Ok(map_image) => {
                            match self.resolve_map_image(user_ns, &map_image, map_prompt.as_deref().unwrap_or_default()).await {
                                Ok(url) => {
                                    memory.map = Some(AiBookMap {
                                        image_url: Some(url),
                                        prompt: map_prompt.clone(),
                                        updated_at: Some(now_ts() * 1000),
                                        source_chapter_index: Some(index),
                                        fallback: None,
                                        fallback_reason: None,
                                    });
                                    memory.map_dirty = false;
                                }
                                Err(reason) => apply_map_fallback(&mut memory, map_prompt.as_deref(), &reason.to_string(), Some(index)),
                            }
                        }
                        Err(reason) => {
                            tracing::warn!(target: SIDECAR_LOG_TARGET, "地图生成失败，降级关系图：{reason:#}");
                            apply_map_fallback(&mut memory, map_prompt.as_deref(), &reason.to_string(), Some(index));
                        }
                    }
                }

                self.update_snapshot(|snapshot| {
                    snapshot.phase = "saving".to_string();
                    snapshot.status_text = "保存 AI 资料...".to_string();
                })
                .await;
                self.persist(user_ns, &memory).await?;
                self.update_snapshot(|snapshot| {
                    snapshot.status_text = format!("已更新至第 {} 章", index + 1);
                })
                .await;
            }
            Ok(format!("已更新至第 {} 章", target + 1))
        }
        .await;

        // 关闭 stdin → sidecar 在 EOF 后自行退出；进程由 task.child 兜底 kill
        let _ = stdin.shutdown().await;
        // 取消（kill 进程）会让帧泵以「sidecar 意外退出」收场——按用户意图改写
        if task.cancel.load(Ordering::SeqCst) {
            return Ok("已取消".to_string());
        }
        outcome
    }

    async fn execute_redraw_task(
        &self,
        user_ns: &str,
        book: &Book,
        job_id: &str,
        task: &ActiveTask,
    ) -> Result<String, AppError> {
        let mut memory = self
            .ai_book_service
            .get(user_ns, book.book_id.as_deref().unwrap_or(""))
            .await?
            .unwrap_or_else(|| default_memory(book));
        let model = self.ai_model_service.get().await?;
        let image_endpoint = model.resolve(AiModelKind::Image);
        if !image_endpoint.enabled
            || image_endpoint.base_url.trim().is_empty()
            || image_endpoint.model.trim().is_empty()
        {
            return Err(AppError::BadRequest("图片模型未配置".to_string()));
        }
        let text_endpoint = model.resolve(AiModelKind::Text);
        let model_json = model_payload_json(&text_endpoint, &image_endpoint);

        let (child, mut stdin, mut rx, stderr_tail) = self.spawn_sidecar().await?;
        *task.child.lock().await = Some(child);
        let outcome = async {
            let frame = json!({
                "type": "run",
                "job": {
                    "jobId": job_id,
                    "kind": "redraw_map",
                    "book": {"name": book.name, "author": book.author, "bookUrl": book.book_url},
                    "memory": serde_json::to_value(&memory).unwrap_or_default(),
                    "model": model_json,
                }
            });
            self.update_snapshot(|snapshot| {
                snapshot.phase = "map".to_string();
                snapshot.status_text = "生成世界地图...".to_string();
            })
            .await;
            let ctx = TaskContext {
                user_ns,
                book,
                chapters: &[],
                memory: &memory,
            };
            let result = self
                .run_frame_until_result(&mut stdin, &mut rx, frame, job_id, &ctx, &stderr_tail)
                .await?;
            // 与前端旧版 redrawMap 的语义一致：生成/落盘失败不作为任务错误，
            // 而是降级为关系图兜底并保存
            let redraw_result: Result<(String, String), AppError> = async {
                let map_image = decode_map_image(&result)?;
                let prompt = result
                    .get("mapPrompt")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let url = self.resolve_map_image(user_ns, &map_image, &prompt).await?;
                Ok((url, prompt))
            }
            .await;
            match redraw_result {
                Ok((url, prompt)) => {
                    let index = memory.processed_chapter_index;
                    memory.map = Some(AiBookMap {
                        image_url: Some(url),
                        prompt: Some(prompt),
                        updated_at: Some(now_ts() * 1000),
                        source_chapter_index: index,
                        fallback: None,
                        fallback_reason: None,
                    });
                    memory.map_dirty = false;
                    memory.updated_at = now_ts() * 1000;
                    self.persist(user_ns, &memory).await?;
                    Ok("地图已更新".to_string())
                }
                Err(reason) => {
                    tracing::warn!(target: SIDECAR_LOG_TARGET, "地图生成失败，降级关系图：{reason:#}");
                    let prompt = result
                        .get("mapPrompt")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| memory.map.as_ref().and_then(|m| m.prompt.clone()))
                        .unwrap_or_default();
                    let source_index = memory.processed_chapter_index;
                    apply_map_fallback(&mut memory, Some(&prompt), &reason.to_string(), source_index);
                    memory.updated_at = now_ts() * 1000;
                    self.persist(user_ns, &memory).await?;
                    Ok("图片地图不可用，已显示关系图".to_string())
                }
            }
        }
        .await;
        let _ = stdin.shutdown().await;
        // 与批量路径一致：取消（kill 进程）不是错误
        if task.cancel.load(Ordering::SeqCst) {
            return Ok("已取消".to_string());
        }
        outcome
    }

    // -----------------------------------------------------------------------
    // 帧泵：发 run 帧 → 处理 status/round/tool_call → 收 result
    // -----------------------------------------------------------------------

    async fn run_frame_until_result(
        &self,
        stdin: &mut ChildStdin,
        rx: &mut mpsc::Receiver<Result<String, String>>,
        run_frame: Value,
        expected_job_id: &str,
        ctx: &TaskContext<'_>,
        stderr_tail: &StderrTail,
    ) -> Result<Value, AppError> {
        // NDJSON：帧必须以换行结尾，对端按行读取
        let mut line =
            serde_json::to_string(&run_frame).map_err(|e| AppError::Internal(e.into()))?;
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        stdin
            .flush()
            .await
            .map_err(|e| AppError::Internal(e.into()))?;

        let wait = timeout(self.chapter_timeout, async {
            while let Some(raw) = rx.recv().await {
                // stdout 泵的致命错误（读取失败/单行超限）：中止等待，
                // 进程由调用方的清理路径杀掉
                let raw = match raw {
                    Ok(line) => line,
                    Err(message) => return Err(AppError::Internal(anyhow::anyhow!(message))),
                };
                let frame: Value = match serde_json::from_str(&raw) {
                    Ok(value) => value,
                    Err(_) => {
                        tracing::warn!(target: SIDECAR_LOG_TARGET, "忽略非 JSON 输出行（{} 字节）", raw.len());
                        continue;
                    }
                };
                match frame.get("type").and_then(Value::as_str) {
                    Some("status") => {
                        let phase = frame.get("phase").and_then(Value::as_str).unwrap_or("text");
                        let message = frame.get("message").and_then(Value::as_str).unwrap_or("");
                        self.update_snapshot(|snapshot| {
                            snapshot.phase = phase.to_string();
                            snapshot.status_text = message.to_string();
                        })
                        .await;
                    }
                    Some("round") => {
                        let step = frame.get("step").and_then(Value::as_i64).unwrap_or(0);
                        let tool_names = frame.get("toolCalls").cloned().unwrap_or(json!([]));
                        tracing::debug!(target: SIDECAR_LOG_TARGET, "round {step} toolCalls={tool_names}");
                    }
                    Some("tool_call") => {
                        // jobId 归属校验（协议 v2 起 tool_call 必带）：同一进程
                        // 同一信道上可能混着迟到帧（典型是上一章 redraw 超时降级
                        // 后迟到的工具调用）。不匹配的调用不能放行——回一个带
                        // 错误内容的 tool_result 让 sidecar 感知，但任务不中断
                        let frame_job_id = frame.get("jobId").and_then(Value::as_str);
                        let content = if frame_job_id != Some(expected_job_id) {
                            // 宏外求值：tracing 宏内 Value 会解析成 field::Value trait
                            let tool_name = frame.get("name").and_then(Value::as_str).unwrap_or("?");
                            tracing::warn!(
                                target: SIDECAR_LOG_TARGET,
                                "拒绝 jobId 不匹配的 tool_call：期望 {expected_job_id}，收到 {frame_job_id:?}（工具 {tool_name}）"
                            );
                            json!({
                                "ok": false,
                                "error": format!("工具调用被拒绝：jobId 与当前任务不匹配（期望 {expected_job_id}）"),
                            })
                        } else {
                            self.answer_tool_call(ctx, &frame).await
                        };
                        let response = json!({
                            "type": "tool_result",
                            "id": frame.get("id").cloned().unwrap_or(Value::Null),
                            "jobId": expected_job_id,
                            "content": content,
                        });
                        let mut response_line =
                            serde_json::to_string(&response).map_err(|e| AppError::Internal(e.into()))?;
                        response_line.push('\n');
                        stdin
                            .write_all(response_line.as_bytes())
                            .await
                            .map_err(|e| AppError::Internal(e.into()))?;
                        stdin.flush().await.map_err(|e| AppError::Internal(e.into()))?;
                    }
                    Some("result") => {
                        // jobId 校验：同一进程同一信道上可能混着迟到帧（典型是
                        // 上一章 redraw 超时降级后迟到的 result）。不匹配就跳过
                        // 继续等，绝不能把别的 job 的结果当成当前章节的——
                        // 地图 result 没有 memory 字段，误收会炸「result 帧缺少 memory」
                        let frame_job_id = frame.get("jobId").and_then(Value::as_str).unwrap_or("");
                        if frame_job_id != expected_job_id {
                            tracing::warn!(
                                target: SIDECAR_LOG_TARGET,
                                "忽略 jobId 不匹配的 result 帧：期望 {expected_job_id}，收到 {frame_job_id:?}"
                            );
                            continue;
                        }
                        return Ok(frame);
                    }
                    other => {
                        tracing::warn!(target: SIDECAR_LOG_TARGET, "未知帧类型：{:?}", other);
                    }
                }
            }
            // EOF：sidecar 崩溃/被杀——把它的 stderr 末尾带进错误。
            // 在 async 上下文里必须 .lock().await：tokio 的 blocking_lock 在运行时内会
            // panic，panic 沿 spawn 展开会跳过 start_task 尾部的清理，active 槽永久占用。
            let tail = stderr_tail.lock().await;
            if tail.is_empty() {
                Err(AppError::Internal(anyhow::anyhow!("sidecar 意外退出")))
            } else {
                Err(AppError::Internal(anyhow::anyhow!(
                    "sidecar 意外退出；子进程 stderr 末尾：{}",
                    tail.join(" ┃ ")
                )))
            }
        })
        .await;

        match wait {
            Ok(inner) => inner,
            Err(_) => Err(AppError::BadRequest(format!(
                "章节处理超时（{} 秒）",
                self.chapter_timeout.as_secs()
            ))),
        }
    }

    // -----------------------------------------------------------------------
    // 工具应答（统一内容链路：本地 TXT/EPUB + 在线书源）
    // -----------------------------------------------------------------------

    async fn answer_tool_call(&self, ctx: &TaskContext<'_>, frame: &Value) -> Value {
        let name = frame.get("name").and_then(Value::as_str).unwrap_or("");
        let args = frame.get("args").cloned().unwrap_or(json!({}));
        let result = match name {
            "get_completed_chapter" => self.tool_get_chapter(ctx, &args).await,
            "search_read_content" => self.tool_search_content(ctx, &args).await,
            other => Ok(json!({"ok": false, "error": format!("未知工具：{other}")})),
        };
        match result {
            Ok(value) => value,
            Err(error) => json!({"ok": false, "error": error.to_string()}),
        }
    }

    async fn tool_get_chapter(
        &self,
        ctx: &TaskContext<'_>,
        args: &Value,
    ) -> Result<Value, AppError> {
        let index = args
            .get("index")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0) as usize;
        let chapter = ctx
            .chapters
            .get(index)
            .ok_or_else(|| AppError::BadRequest(format!("章节 index {index} 不存在")))?;
        let content = self
            .fetch_chapter_content(ctx.user_ns, ctx.book, chapter)
            .await?;
        Ok(json!({
            "ok": true,
            "content": content,
            "totalLength": content.chars().count(),
        }))
    }

    async fn tool_search_content(
        &self,
        ctx: &TaskContext<'_>,
        args: &Value,
    ) -> Result<Value, AppError> {
        let keyword = args
            .get("keyword")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| AppError::BadRequest("keyword 不能为空".to_string()))?;
        // 服务端再钳一次：搜索范围不得超过当前已处理进度（不剧透约束不靠 sidecar 自觉）
        let processed = ctx.memory.processed_chapter_index.unwrap_or(-1);
        let from_index = args
            .get("fromIndex")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0) as i32;
        let to_index = args
            .get("toIndex")
            .and_then(Value::as_i64)
            .map(|value| value as i32)
            .unwrap_or(processed)
            .min(processed)
            .min((ctx.chapters.len() as i32) - 1);
        if from_index > to_index {
            return Ok(json!({"ok": false, "error": "搜索范围超出已读进度"}));
        }

        let mut matches: Vec<Value> = Vec::new();
        let mut scanned: usize = 0;
        let mut index = from_index;
        while index <= to_index
            && scanned < SEARCH_MAX_CHAPTERS
            && matches.len() < SEARCH_MAX_SNIPPETS
        {
            scanned += 1;
            let chapter = &ctx.chapters[index as usize];
            if let Ok(content) = self
                .fetch_chapter_content(ctx.user_ns, ctx.book, chapter)
                .await
            {
                if let Some(snippet) = find_snippet(&content, keyword) {
                    matches.push(json!({
                        "chapterIndex": index,
                        "chapterTitle": chapter.title,
                        "snippet": snippet,
                    }));
                }
            }
            index += 1;
        }
        Ok(json!({
            "ok": true,
            "matches": matches,
            "range": {"fromIndex": from_index, "toIndex": to_index},
        }))
    }

    /// 章节正文：本地 TXT/EPUB 与在线书源统一走 getBookContent 的内容链路。
    async fn fetch_chapter_content(
        &self,
        user_ns: &str,
        book: &Book,
        chapter: &BookChapter,
    ) -> Result<String, AppError> {
        if is_local_txt_url(&chapter.url) || is_local_txt_origin(&book.origin) {
            let url = ensure_local_chapter_index(&chapter.url, chapter.index);
            return self.local_txt.get_content(user_ns, &url).await;
        }
        if is_local_epub_url(&chapter.url) || is_local_epub_origin(&book.origin) {
            let url = ensure_local_chapter_index(&chapter.url, chapter.index);
            return self.local_epub.get_content(user_ns, &url).await;
        }
        let source = self
            .resolve_source(user_ns, book)
            .await?
            .ok_or_else(|| AppError::BadRequest("书源不存在".to_string()))?;
        self.book_service
            .get_content(
                user_ns,
                book.book_id.as_deref().unwrap_or(""),
                &source,
                &repair_encoded_url(&chapter.url),
            )
            .await
    }

    /// 目录：本地书走本地索引，在线书走带缓存目录。
    async fn load_chapter_list(
        &self,
        user_ns: &str,
        book: &Book,
    ) -> Result<Vec<BookChapter>, AppError> {
        if is_local_txt_url(&book.book_url) || is_local_txt_origin(&book.origin) {
            return self
                .local_txt
                .get_chapter_list(user_ns, &book.book_url)
                .await;
        }
        if is_local_epub_url(&book.book_url) || is_local_epub_origin(&book.origin) {
            return self
                .local_epub
                .get_chapter_list(user_ns, &book.book_url)
                .await;
        }
        let source = self
            .resolve_source(user_ns, book)
            .await?
            .ok_or_else(|| AppError::BadRequest("书源不存在".to_string()))?;
        let book_id = book.book_id.clone().unwrap_or_default();
        let toc_url = match book.toc_url.as_deref().filter(|v| !v.trim().is_empty()) {
            Some(toc_url) => toc_url.to_string(),
            // 书架条目可能不持久化 tocUrl（部分书源的目录地址由书籍信息规则
            // 动态算出，如 FQWeb 的 info→catalog）——与 getChapterList handler
            // 对齐：缺 tocUrl 时先经 book_info 回退拿真实目录地址
            None => self
                .book_service
                .get_book_info(user_ns, &book_id, &source, &book.book_url, false)
                .await?
                .toc_url
                .as_deref()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or(&book.book_url)
                .to_string(),
        };
        self.book_service
            .get_chapter_list_with_cache(
                user_ns,
                &book_id,
                &source,
                &repair_encoded_url(&toc_url),
                false,
            )
            .await
    }

    async fn resolve_source(
        &self,
        user_ns: &str,
        book: &Book,
    ) -> Result<Option<BookSource>, AppError> {
        match book.book_source_id {
            Some(id) => self.book_source_service.get_by_id(user_ns, id).await,
            None => Ok(None),
        }
    }

    // -----------------------------------------------------------------------
    // 地图：生成（经 sidecar）+ 落盘（b64 解码或 URL 下载）
    // -----------------------------------------------------------------------

    async fn redraw_map_via_sidecar(
        &self,
        stdin: &mut ChildStdin,
        rx: &mut mpsc::Receiver<Result<String, String>>,
        memory: &AiBookMemory,
        model_json: &Value,
        stderr_tail: &StderrTail,
        frame_job_id: &str,
    ) -> Result<MapImage, AppError> {
        let frame = json!({
            "type": "run",
            "job": {
                "jobId": frame_job_id,
                "kind": "redraw_map",
                "memory": serde_json::to_value(memory).unwrap_or_default(),
                "model": model_json,
            }
        });
        let ctx = TaskContext {
            user_ns: "",
            book: &Book::default(),
            chapters: &[],
            memory,
        };
        let result = self
            .run_frame_until_result(stdin, rx, frame, frame_job_id, &ctx, stderr_tail)
            .await?;
        decode_map_image(&result)
    }

    async fn resolve_map_image(
        &self,
        user_ns: &str,
        map_image: &MapImage,
        prompt: &str,
    ) -> Result<String, AppError> {
        let bytes = match map_image {
            MapImage::Bytes(bytes) => bytes.clone(),
            MapImage::Url(url) => download_map_image(url).await?,
        };
        if bytes.is_empty() {
            return Err(AppError::BadRequest("地图图片内容为空".to_string()));
        }
        let digest = md5_hex(prompt);
        let name = format!("{}-{}.png", now_ts(), &digest[..8]);
        write_asset_file(&self.assets_dir, user_ns, "ai-maps", &name, bytes).await
    }

    // -----------------------------------------------------------------------
    // 注册表与持久化
    // -----------------------------------------------------------------------

    async fn persist(&self, user_ns: &str, memory: &AiBookMemory) -> Result<(), AppError> {
        let book_id = memory
            .book_id
            .clone()
            .ok_or_else(|| AppError::BadRequest("bookId required".to_string()))?;
        self.ai_book_service
            .save_for_book(user_ns, &book_id, &memory.book_url, memory.clone())
            .await?;
        Ok(())
    }

    async fn update_snapshot(&self, f: impl FnOnce(&mut AgentTaskSnapshot)) {
        let mut registry = self.registry.lock().await;
        if let Some(snapshot) = registry.as_mut() {
            f(snapshot);
        }
    }

    // -----------------------------------------------------------------------
    // sidecar 进程
    // -----------------------------------------------------------------------

    async fn spawn_sidecar(
        &self,
    ) -> Result<
        (
            Child,
            ChildStdin,
            mpsc::Receiver<Result<String, String>>,
            StderrTail,
        ),
        AppError,
    > {
        let plan = self.spawn_plan().await;
        let (program, args, handshake_timeout) = match plan {
            // uv 自动管理：uv run --project <sidecar_dir> python -m agent_sidecar
            SpawnPlan::Uv(uv) => (uv, uv_run_args(&self.sidecar_dir), HANDSHAKE_TIMEOUT_UV),
            SpawnPlan::Custom(program, args) => (program, args, HANDSHAKE_TIMEOUT),
            SpawnPlan::Default(mut program, args) => {
                // 开发便利：sidecar/.venv 存在时优先用它的 Python——
                // 系统 Python 往往没有 sidecar 依赖（ModuleNotFoundError 是
                // 最常见的握手失败原因）。uv 与自定义命令不受影响。
                if let Some(venv_python) = venv_python(&self.sidecar_dir) {
                    tracing::info!(
                        target: SIDECAR_LOG_TARGET,
                        "使用 sidecar venv 的 Python：{}",
                        venv_python.display()
                    );
                    program = venv_python.to_string_lossy().to_string();
                }
                (program, args, HANDSHAKE_TIMEOUT)
            }
        };
        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .current_dir(&self.sidecar_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // 环境变量白名单化：子进程不继承宿主的服务端凭据
        // （JWT_SECRET / ADMIN_PASSWORD / AI_* API Key / DATABASE_URL 等）
        cmd.env_clear();
        for (key, value) in sidecar_env_allowlist() {
            cmd.env(key, value);
        }
        // 模型总预算显式下发（白名单虽放行 AGENT_SIDECAR_ 前缀，但这里用
        // 归一化后的配置值覆盖父进程原值；三条 spawn 路径共用这一段 env 设置）
        cmd.env(
            "AGENT_SIDECAR_MODEL_BUDGET_SECS",
            self.model_budget_secs.to_string(),
        );
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn().map_err(|e| {
            AppError::BadRequest(format!(
                "sidecar 启动失败（{program}）：{e}。请安装 Python ≥3.11 与 sidecar 依赖，                 或安装 uv 让依赖自动管理（可用 AGENT_SIDECAR_UV 指定路径）"
            ))
        })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("sidecar stdout 不可用")))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("sidecar stderr 不可用")))?;

        // stderr 持续排空进 tracing——不排空会因管道背压把 sidecar 卡死；
        // 同时保留末尾几行，握手失败时把真实原因（如缺依赖、Store 占位符）带进错误
        let stderr_tail = Arc::new(AsyncMutex::new(Vec::<String>::new()));
        let tail_for_drain = stderr_tail.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: SIDECAR_LOG_TARGET, "{line}");
                let mut tail = tail_for_drain.lock().await;
                if tail.len() >= STDERR_TAIL_LINES {
                    tail.remove(0);
                }
                tail.push(line.chars().take(STDERR_LINE_CHARS).collect());
            }
        });

        // stdout 逐行转 mpsc；单行长度上限（见 read_line_capped）——失控
        // sidecar 不能靠一行超长 NDJSON 把宿主内存推高。读取出错/超限以
        // Err 带给帧泵，让调用方中止任务（进程由 kill_on_drop 与任务清理杀掉）
        let (tx, rx) = mpsc::channel::<Result<String, String>>(16);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line: Vec<u8> = Vec::with_capacity(8 * 1024);
            loop {
                match read_line_capped(&mut reader, &mut line, MAX_NDJSON_LINE_BYTES).await {
                    Ok(true) => {
                        let text = String::from_utf8_lossy(&line);
                        if tx.send(Ok(text.trim_end().to_string())).await.is_err() {
                            break;
                        }
                    }
                    Ok(false) => break, // EOF
                    Err(message) => {
                        tracing::warn!(target: SIDECAR_LOG_TARGET, "{message}");
                        let _ = tx.send(Err(message)).await;
                        break;
                    }
                }
            }
        });

        // 握手：hello 帧 → 协议版本。失败必须带上子进程 stderr 的真实原因，
        // 否则用户只能看到一句无法定位的 "internal error"
        let mut rx = rx;
        let first_line = match timeout(handshake_timeout, rx.recv()).await {
            Ok(Some(Ok(line))) => Some(line),
            Ok(Some(Err(message))) => {
                return Err(handshake_failed(&stderr_tail, &message).await);
            }
            Ok(None) => None,
            Err(_) => {
                return Err(handshake_failed(
                    &stderr_tail,
                    &format!("sidecar 握手超时（{} 秒）", handshake_timeout.as_secs()),
                )
                .await)
            }
        };
        let Some(first_line) = first_line else {
            return Err(handshake_failed(&stderr_tail, "sidecar 在握手前就退出了").await);
        };
        let hello: Value = match serde_json::from_str(&first_line) {
            Ok(value) => value,
            Err(e) => {
                return Err(
                    handshake_failed(&stderr_tail, &format!("sidecar 握手帧无效：{e}")).await,
                )
            }
        };
        if hello.get("protocol").and_then(Value::as_u64) != Some(PROTOCOL_VERSION as u64) {
            return Err(AppError::BadRequest(format!(
                "sidecar 协议版本不匹配：期望 {PROTOCOL_VERSION}，收到 {:?}",
                hello.get("protocol")
            )));
        }

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("sidecar stdin 不可用")))?;
        Ok((child, stdin, rx, stderr_tail))
    }
}

/// 地图图片的两种形态：sidecar 返回 b64 或上游 URL。
enum MapImage {
    Bytes(Vec<u8>),
    Url(String),
}

/// 逐块读一行（含结尾换行符），单行超过 `limit` 字节返回明确错误。
///
/// 用 fill_buf/consume 增量扫描而非 `lines()`/`read_until`——后两者都会先把
/// 整行无界读进内存才轮得到长度检查。返回 `Ok(false)` 表示 EOF（`line` 为空）；
/// EOF 前的最后一行即使没有换行符也会返回。内存占用以 `limit` 为上界。
async fn read_line_capped(
    reader: &mut BufReader<impl tokio::io::AsyncRead + Unpin>,
    line: &mut Vec<u8>,
    limit: usize,
) -> Result<bool, String> {
    line.clear();
    loop {
        let used = {
            let buffered = match reader.fill_buf().await {
                Ok(buffered) => buffered,
                Err(e) => return Err(format!("读取 sidecar stdout 失败：{e}")),
            };
            if buffered.is_empty() {
                return Ok(!line.is_empty());
            }
            match buffered.iter().position(|&b| b == b'\n') {
                Some(pos) => pos + 1,
                None => buffered.len(),
            }
        };
        // 先查限再拷贝：超限时不把这批字节追加进内存
        if line.len() + used > limit {
            return Err(format!(
                "sidecar 输出单行超过 {} MB 上限（>{} 字节），判定对端失控",
                limit / (1024 * 1024),
                limit
            ));
        }
        {
            let buffered = reader
                .fill_buf()
                .await
                .map_err(|e| format!("读取 sidecar stdout 失败：{e}"))?;
            line.extend_from_slice(&buffered[..used]);
        }
        reader.consume(used);
        if line.last() == Some(&b'\n') {
            return Ok(true);
        }
    }
}

fn decode_map_image(result: &Value) -> Result<MapImage, AppError> {
    if result.get("ok").and_then(Value::as_bool) != Some(true) {
        let error = result
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("地图生成失败");
        return Err(AppError::BadRequest(error.to_string()));
    }
    let map = result.get("map").cloned().unwrap_or(json!({}));
    if let Some(b64) = map
        .get("b64Json")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| AppError::BadRequest(format!("地图 b64 解码失败：{e}")))?;
        if bytes.len() as u64 > MAX_MAP_IMAGE_BYTES {
            return Err(AppError::BadRequest("地图图片超过大小上限".to_string()));
        }
        return Ok(MapImage::Bytes(bytes));
    }
    if let Some(url) = map
        .get("imageUrl")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    {
        return Ok(MapImage::Url(url.to_string()));
    }
    Err(AppError::BadRequest("地图生成结果为空".to_string()))
}

async fn download_map_image(url: &str) -> Result<Vec<u8>, AppError> {
    let parsed = url::Url::parse(url)
        .map_err(|e| AppError::BadRequest(format!("地图图片 URL 无效：{e}")))?;
    crate::crawler::url_guard::ensure_outbound_url_allowed(&parsed)
        .await
        .map_err(AppError::BadRequest)?;
    // 与书源抓取同一套受守卫客户端：预检只管到第一跳，302 跳内网 /
    // DNS rebinding（预检后重新解析出不同结果）要靠逐跳重定向策略与
    // 解析期守卫兜底，自建裸 Client 会把这两扇窗都敞开
    let client = crate::crawler::http_client::build_guarded_client(
        MAP_IMAGE_DOWNLOAD_TIMEOUT.as_secs().max(1),
        None,
        crate::crawler::http_client::DEFAULT_USER_AGENT,
    )
    .map_err(AppError::Internal)?;
    let response = client.get(url).send().await?;
    if let Some(length) = response.content_length() {
        if length > MAX_MAP_IMAGE_BYTES {
            return Err(AppError::BadRequest("地图图片超过大小上限".to_string()));
        }
    }
    // 边收边限量：Content-Length 缺失/伪造（chunked 流）时 .bytes() 会把整个
    // 失控上游先读进内存才轮得到大小判断
    let bytes = crate::crawler::fetcher::read_body_limited(response, MAX_MAP_IMAGE_BYTES)
        .await
        .map_err(|e| AppError::BadRequest(format!("地图图片超过大小上限：{e}")))?;
    Ok(bytes.to_vec())
}

fn model_payload_json(text: &ResolvedAiModelEndpoint, image: &ResolvedAiModelEndpoint) -> Value {
    json!({
        "text": endpoint_json(text),
        "image": endpoint_json(image),
    })
}

fn endpoint_json(endpoint: &ResolvedAiModelEndpoint) -> Value {
    let mut value = json!({
        "baseUrl": endpoint.base_url,
        "apiKey": endpoint.api_key,
        "model": endpoint.model,
        "useFullUrl": endpoint.use_full_url,
    });
    if let Some(size) = &endpoint.image_size {
        value["imageSize"] = Value::String(size.clone());
    }
    value
}

fn default_memory(book: &Book) -> AiBookMemory {
    AiBookMemory {
        book_id: book.book_id.clone(),
        book_url: book.book_url.clone(),
        book_name: Some(book.name.clone()),
        author: Some(book.author.clone()),
        enabled: true,
        updated_at: now_ts() * 1000,
        ..AiBookMemory::default()
    }
}

fn chapters_window(chapters: &[BookChapter], index: i32) -> Vec<Value> {
    let len = chapters.len() as i32;
    let (start, end) = if index <= CHAPTER_WINDOW_NEAR_START {
        (0, (index + 21).min(len))
    } else {
        ((index - 10).max(0), (index + 11).min(len))
    };
    if start >= end {
        return Vec::new();
    }
    chapters[start as usize..end as usize]
        .iter()
        .map(|chapter| json!({"index": chapter.index, "title": chapter.title}))
        .collect()
}

fn ensure_local_chapter_index(url: &str, index: i32) -> String {
    let repaired = repair_encoded_url(url);
    if repaired.contains('#') {
        repaired
    } else {
        format!(
            "{}#{}",
            repaired.trim_end_matches('#'),
            index.max(0) as usize
        )
    }
}

/// 复刻前端 applyMapFallbackToMemory 的字段结构：fallback 标识 +
/// mapDirty=true（「地图待重绘」，get_memory 会把它暴露给模型）。
fn apply_map_fallback(
    memory: &mut AiBookMemory,
    prompt: Option<&str>,
    reason: &str,
    source_chapter_index: Option<i32>,
) {
    memory.map = Some(AiBookMap {
        image_url: None,
        prompt: prompt.map(str::to_string),
        updated_at: Some(now_ts() * 1000),
        source_chapter_index,
        fallback: Some("relationship-graph".to_string()),
        fallback_reason: Some(format!("{reason}，已显示关系图")),
    });
    memory.map_dirty = true;
}

/// 在正文中定位关键词并截取片段（字符级切分，避免 UTF-8 边界问题）。
fn find_snippet(content: &str, keyword: &str) -> Option<String> {
    let chars: Vec<char> = content.chars().collect();
    let keyword_chars: Vec<char> = keyword.chars().collect();
    if keyword_chars.is_empty() || chars.len() < keyword_chars.len() {
        return None;
    }
    let position = (0..=chars.len() - keyword_chars.len())
        .find(|&start| chars[start..start + keyword_chars.len()] == keyword_chars[..])?;
    let snippet_start = position.saturating_sub(SEARCH_CONTEXT_BEFORE);
    let snippet_end = (snippet_start + SEARCH_SNIPPET_CHARS).min(chars.len());
    let mut snippet: String = chars[snippet_start..snippet_end].iter().collect();
    if snippet_start > 0 {
        snippet.insert(0, '…');
    }
    if snippet_end < chars.len() {
        snippet.push('…');
    }
    Some(snippet)
}

/// 启动计划的优先级解析（纯函数，规则与 spec 一致）：
///
/// 1. `AGENT_SIDECAR_COMMAND` 显式非默认 → 原样使用（测试与自托管逃生门）；
/// 2. `AGENT_SIDECAR_UV` 非空 → uv 用该路径；
/// 3. PATH 里有 `uv` → 用 uv；
/// 4. 都没有 → 默认命令降级（宿主机需已装好依赖）。
///
/// 注意 uv 路径来自 env，可能含空格——它是单个二进制路径，绝不能过 split_command。
fn resolve_spawn_plan(command: &str, uv_path: &str, uv_in_path: bool) -> SpawnPlan {
    if command != DEFAULT_SIDECAR_COMMAND {
        if let Some((program, args)) = split_command(command) {
            return SpawnPlan::Custom(program, args);
        }
    }
    let uv_path = uv_path.trim();
    if !uv_path.is_empty() {
        return SpawnPlan::Uv(uv_path.to_string());
    }
    if uv_in_path {
        return SpawnPlan::Uv("uv".to_string());
    }
    match split_command(command) {
        Some((program, args)) => SpawnPlan::Default(program, args),
        None => SpawnPlan::Default(
            "python".to_string(),
            vec!["-m".into(), "agent_sidecar".into()],
        ),
    }
}

/// uv run 的参数形态：--project 指向 sidecar 目录（uv 会按需同步 .venv）。
fn uv_run_args(sidecar_dir: &std::path::Path) -> Vec<String> {
    vec![
        "run".to_string(),
        "--project".to_string(),
        sidecar_dir.to_string_lossy().to_string(),
        "python".to_string(),
        "-m".to_string(),
        "agent_sidecar".to_string(),
    ]
}

/// PATH 里的 `uv` 是否可执行（只探测一次，结果在 spawn_plan 缓存）。
async fn uv_in_path() -> bool {
    tokio::process::Command::new("uv")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

/// 按空白切分命令，支持双引号包裹含空格的路径。
fn split_command(command: &str) -> Option<(String, Vec<String>)> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in trimmed.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    if parts.is_empty() {
        return None;
    }
    let program = parts.remove(0);
    Some((program, parts))
}

/// 任务快照里的错误文案：Internal 用 anyhow 错误链（Display 只有顶层一句 "internal error"），
/// 其余保持 Display。给「sidecar 意外退出」这类内部错误一个能被看懂的上下文。
fn task_error_text(error: &AppError) -> String {
    match error {
        AppError::Internal(e) => format!("内部错误：{e:#}"),
        _ => error.to_string(),
    }
}

/// 握手失败：附上子进程 stderr 末尾（缺依赖、Store 占位符等真实原因都在那里）。
async fn handshake_failed(stderr_tail: &StderrTail, reason: &str) -> AppError {
    let tail = stderr_tail.lock().await;
    let detail = if tail.is_empty() {
        String::new()
    } else {
        format!("；子进程 stderr 末尾：{}", tail.join(" ┃ "))
    };
    AppError::BadRequest(format!("{reason}{detail}"))
}

/// `sidecar/.venv` 里的解释器路径（存在才返回 Some）。
fn venv_python(sidecar_dir: &std::path::Path) -> Option<PathBuf> {
    #[cfg(windows)]
    let candidate = sidecar_dir.join(".venv").join("Scripts").join("python.exe");
    #[cfg(not(windows))]
    let candidate = sidecar_dir.join(".venv").join("bin").join("python");
    candidate.is_file().then_some(candidate)
}

fn resolve_sidecar_dir() -> PathBuf {
    // 必须返回绝对路径：spawn 时 current_dir 就是 sidecar/，`uv --project` 等参数
    // 若拿到相对路径会被双重相对（sidecar/sidecar）而找不到目录
    let candidates: Vec<PathBuf> = {
        let mut list = vec![PathBuf::from("sidecar")];
        if let Ok(exe) = std::env::current_exe() {
            if let Some(parent) = exe.parent() {
                list.push(parent.join("sidecar"));
            }
        }
        list
    };
    for candidate in &candidates {
        if candidate.join("agent_sidecar").is_dir() {
            return std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.clone());
        }
    }
    std::env::current_dir()
        .map(|cwd| cwd.join("sidecar"))
        .unwrap_or_else(|_| PathBuf::from("sidecar"))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::crawler::url_guard::{
        restore_guard_defaults, set_allow_private_network, TEST_SERIALIZER,
    };
    use tokio::io::AsyncReadExt;

    fn plan_command(plan: &SpawnPlan) -> (String, Vec<String>) {
        match plan {
            SpawnPlan::Custom(p, a) | SpawnPlan::Default(p, a) => (p.clone(), a.clone()),
            SpawnPlan::Uv(uv) => (uv.clone(), Vec::new()),
        }
    }

    #[test]
    fn explicit_custom_command_wins_over_uv() {
        // 逃生门：显式命令（测试的 node 假 sidecar 也走这里）
        let plan = resolve_spawn_plan("node fake.mjs", "C:/tools/uv.exe", true);
        let (program, args) = plan_command(&plan);
        assert_eq!(program, "node");
        assert_eq!(args, vec!["fake.mjs".to_string()]);
        assert!(matches!(plan, SpawnPlan::Custom(..)));
    }

    #[test]
    fn explicit_uv_path_wins_over_path_lookup() {
        let plan = resolve_spawn_plan(DEFAULT_SIDECAR_COMMAND, "C:/tools/uv.exe", true);
        assert!(matches!(plan, SpawnPlan::Uv(ref p) if p == "C:/tools/uv.exe"));
    }

    #[test]
    fn uv_in_path_is_used_when_no_override() {
        let plan = resolve_spawn_plan(DEFAULT_SIDECAR_COMMAND, "", true);
        assert!(matches!(plan, SpawnPlan::Uv(ref p) if p == "uv"));
    }

    #[test]
    fn default_command_degrades_when_no_uv() {
        let plan = resolve_spawn_plan(DEFAULT_SIDECAR_COMMAND, "", false);
        let (program, args) = plan_command(&plan);
        assert!(matches!(plan, SpawnPlan::Default(..)));
        assert_eq!(program, "python");
        assert_eq!(args, vec!["-m".to_string(), "agent_sidecar".to_string()]);
    }

    #[test]
    fn default_command_parse_failure_falls_back_to_python_module() {
        let plan = resolve_spawn_plan("   ", "", false);
        let (program, args) = plan_command(&plan);
        assert_eq!(program, "python");
        assert_eq!(args, vec!["-m".to_string(), "agent_sidecar".to_string()]);
    }

    #[test]
    fn uv_run_args_point_at_project_python_module() {
        let args = uv_run_args(std::path::Path::new("sidecar"));
        assert_eq!(args[0], "run");
        assert_eq!(args[1], "--project");
        assert_eq!(args[3], "python");
        assert_eq!(args[4..], ["-m".to_string(), "agent_sidecar".to_string()]);
    }

    #[test]
    fn env_allowlist_keeps_minimal_set_and_drops_secrets() {
        let vars = vec![
            ("PATH", "/usr/bin"),
            ("SystemRoot", "C:\\Windows"),
            ("JWT_SECRET", "super-secret"),
            ("ADMIN_PASSWORD", "hunter2"),
            ("AI_TEXT_API_KEY", "sk-text"),
            ("AI_IMAGE_API_KEY", "sk-image"),
            ("DATABASE_URL", "sqlite:storage/reader.db"),
            ("UV_PYTHON", "3.12"),
            ("HTTPS_PROXY", "http://proxy:7890"),
            ("ALL_PROXY", "socks5://proxy:1080"),
            ("TMPDIR", "/var/tmp"),
        ];
        let allowlist = sidecar_env_allowlist_from(vars);
        let names: Vec<&str> = allowlist.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains(&"PATH"), "应保留 PATH：{names:?}");
        assert!(
            names.contains(&"SystemRoot"),
            "应保留 SystemRoot：{names:?}"
        );
        assert!(names.contains(&"UV_PYTHON"), "应保留 UV_ 前缀：{names:?}");
        assert!(names.contains(&"HTTPS_PROXY"), "应保留代理配置：{names:?}");
        assert!(
            names.contains(&"ALL_PROXY"),
            "应保留 ALL_PROXY（httpx/curl 认）：{names:?}"
        );
        assert!(names.contains(&"TMPDIR"), "应保留 TMPDIR：{names:?}");
        for secret in [
            "JWT_SECRET",
            "ADMIN_PASSWORD",
            "AI_TEXT_API_KEY",
            "DATABASE_URL",
        ] {
            assert!(!names.contains(&secret), "{secret} 不应透传：{names:?}");
        }
    }

    #[test]
    fn env_allowlist_from_real_environment_excludes_secrets() {
        // 哨兵变量：即使父进程里有服务端凭据，白名单也不放行
        std::env::set_var("AGENT_TEST_JWT_SECRET_SENTINEL", "leak");
        let allowlist = sidecar_env_allowlist();
        let names: Vec<&str> = allowlist.iter().map(|(k, _)| k.as_str()).collect();
        assert!(
            !names.iter().any(|name| name.contains("SENTINEL")),
            "未知变量不应透传：{names:?}"
        );
        assert!(
            !names
                .iter()
                .any(|name| name.eq_ignore_ascii_case("JWT_SECRET")),
            "JWT_SECRET 不应透传"
        );
        // 编码兜底：UTF-8 必须在（Windows 管道默认 ANSI 代码页会毁掉 NDJSON）
        assert!(names
            .iter()
            .any(|name| name.eq_ignore_ascii_case("PYTHONIOENCODING")));
    }

    #[tokio::test]
    async fn read_line_capped_splits_lines_until_eof() {
        let data: &[u8] = b"{\"a\":1}\n{\"b\":2}\n";
        let mut reader = BufReader::new(data);
        let mut line = Vec::new();
        assert!(read_line_capped(&mut reader, &mut line, 64).await.unwrap());
        assert_eq!(line, b"{\"a\":1}\n");
        assert!(read_line_capped(&mut reader, &mut line, 64).await.unwrap());
        assert_eq!(line, b"{\"b\":2}\n");
        assert!(!read_line_capped(&mut reader, &mut line, 64).await.unwrap());
    }

    #[tokio::test]
    async fn read_line_capped_returns_final_line_without_newline() {
        let data: &[u8] = b"tail";
        let mut reader = BufReader::new(data);
        let mut line = Vec::new();
        assert!(read_line_capped(&mut reader, &mut line, 64).await.unwrap());
        assert_eq!(line, b"tail");
        assert!(!read_line_capped(&mut reader, &mut line, 64).await.unwrap());
    }

    #[tokio::test]
    async fn read_line_capped_rejects_oversized_line() {
        // 65 字节无换行：超过 64 字节上限必须在累积阶段报错
        let data: &[u8] = &[b'x'; 65];
        let mut reader = BufReader::new(data);
        let mut line = Vec::new();
        let error = read_line_capped(&mut reader, &mut line, 64)
            .await
            .unwrap_err();
        assert!(
            error.contains("单行超过"),
            "错误文案应指明单行超限：{error}"
        );
    }

    #[tokio::test]
    async fn read_line_capped_assembles_line_larger_than_buffer() {
        // 行长超过 BufReader 内部缓冲（8KB）：跨块拼行仍正确
        let payload = vec![b'x'; 20 * 1024];
        let mut data = payload.clone();
        data.push(b'\n');
        let mut reader = BufReader::new(&data[..]);
        let mut line = Vec::new();
        assert!(
            read_line_capped(&mut reader, &mut line, MAX_NDJSON_LINE_BYTES)
                .await
                .unwrap()
        );
        assert_eq!(line.len(), 20 * 1024 + 1);
        assert_eq!(&line[..20 * 1024], &payload[..]);
    }

    // ---- download_map_image 出站守卫回归 ----
    //
    // 地图图片地址来自 sidecar（模型输出），属于「外部可控 URL → 服务端发起
    // 请求」入口，必须走受守卫客户端：预检 + 逐跳重定向守卫 + 解析期守卫。
    // 下面用例动 url_guard 的全局状态，一律持 TEST_SERIALIZER 串行（与
    // url_guard 自己的测试同一把锁），并在收尾恢复默认姿态。

    /// 默认（拦截私网）姿态下，环回地址必须在预检阶段被拦下。
    // 持 std Mutex 守卫跨 await：#[tokio::test] 默认 current_thread 运行时，
    // 本线程只有测试自身一个任务，锁竞争只发生在不同测试线程之间，无死锁面
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn download_map_image_blocks_loopback_url_by_default() {
        let _lock = TEST_SERIALIZER.lock().unwrap();
        // 显式回到开箱默认（拦截私网、白名单为空），不依赖上一个持锁测试的收尾
        restore_guard_defaults();

        let err = download_map_image("http://127.0.0.1:1/x.png")
            .await
            .expect_err("默认姿态下环回地址必须被出站守卫拦截");
        match err {
            AppError::BadRequest(msg) => assert!(
                msg.contains("禁止访问内网地址"),
                "错误文案应带拦截原因：{msg}"
            ),
            other => panic!("应为 BadRequest，实际：{other:?}"),
        }

        restore_guard_defaults();
    }

    /// 默认（拦截私网）姿态下，私网 IP 字面量同样在预检阶段被拦下。
    #[allow(clippy::await_holding_lock)] // 理由见 download_map_image_blocks_loopback_url_by_default
    #[tokio::test]
    async fn download_map_image_blocks_private_ip_literal_by_default() {
        let _lock = TEST_SERIALIZER.lock().unwrap();
        restore_guard_defaults();

        for url in [
            "http://192.168.1.1:1/x.png",
            "http://169.254.169.254/latest/meta-data/",
        ] {
            let err = download_map_image(url)
                .await
                .expect_err("默认姿态下私网/链路本地地址必须被出站守卫拦截");
            match err {
                AppError::BadRequest(msg) => assert!(
                    msg.contains("禁止访问内网地址"),
                    "{url} 的错误文案应带拦截原因：{msg}"
                ),
                other => panic!("{url} 应为 BadRequest，实际：{other:?}"),
            }
        }

        restore_guard_defaults();
    }

    /// 显式放行私网后，整条下载链路（预检 → 受守卫客户端 → 限量读取）走通：
    /// 本机临时 HTTP 服务（端口 0 自动分配）返一张 1x1 PNG，字节应原样取回。
    #[allow(clippy::await_holding_lock)] // 理由见 download_map_image_blocks_loopback_url_by_default
    #[tokio::test]
    async fn download_map_image_succeeds_when_private_network_allowed() {
        let _lock = TEST_SERIALIZER.lock().unwrap();
        restore_guard_defaults();
        // 整体放行私网：预检、逐跳重定向守卫与解析期守卫同步放行（见 url_guard）
        set_allow_private_network(true);

        // 1x1 透明 PNG
        const PNG: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x0D, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54,
            0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    // 读掉请求头再回包（内容不校验），Connection: close 收尾
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let mut response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        PNG.len()
                    )
                    .into_bytes();
                    response.extend_from_slice(PNG);
                    let _ = socket.write_all(&response).await;
                });
            }
        });

        let url = format!("http://127.0.0.1:{port}/map.png");
        let bytes = download_map_image(&url)
            .await
            .expect("放行私网后本机服务应下载成功");
        assert_eq!(bytes.as_slice(), PNG);

        server.abort();
        restore_guard_defaults();
    }
}
