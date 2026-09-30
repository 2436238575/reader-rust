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
//! - 单章处理超时（`AGENT_SIDECAR_CHAPTER_TIMEOUT_SECS`）即杀进程中止批量；
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
    util::{
        asset::write_asset_file,
        hash::md5_hex,
        text::{normalize_source_url, repair_encoded_url},
        time::now_ts,
    },
};

const PROTOCOL_VERSION: u32 = 1;
/// 与 config.rs 的默认值保持一致：仅当命令未被自定义时才应用 venv 优先逻辑
const DEFAULT_SIDECAR_COMMAND: &str = "python -m agent_sidecar";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
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
    pub book_url: String,
    pub phase: String,
    pub status_text: String,
    pub current_chapter_index: Option<i32>,
    pub target_chapter_index: Option<i32>,
    pub last_error: Option<String>,
}

impl AgentTaskSnapshot {
    fn empty() -> Self {
        Self {
            running: false,
            job_id: String::new(),
            book_url: String::new(),
            phase: "idle".to_string(),
            status_text: String::new(),
            current_chapter_index: None,
            target_chapter_index: None,
            last_error: None,
        }
    }
}

struct ActiveTask {
    cancel: Arc<AtomicBool>,
    child: Arc<AsyncMutex<Option<Child>>>,
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
    enabled: bool,
    chapter_timeout: Duration,
    sidecar_dir: PathBuf,
    assets_dir: String,
    registry: Arc<AsyncMutex<Option<AgentTaskSnapshot>>>,
    active: Arc<AsyncMutex<Option<Arc<ActiveTask>>>>,
    probe: Arc<AtomicU8>,
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
        enabled: bool,
        chapter_timeout_secs: u64,
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
            enabled,
            chapter_timeout: Duration::from_secs(chapter_timeout_secs.max(1)),
            sidecar_dir: resolve_sidecar_dir(),
            assets_dir: assets_dir.to_string(),
            registry: Arc::new(AsyncMutex::new(None)),
            active: Arc::new(AsyncMutex::new(None)),
            probe: Arc::new(AtomicU8::new(PROBE_UNKNOWN)),
        }
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
            Ok((mut child, mut stdin, _rx)) => {
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
        let digest = md5_hex(&book.book_url);
        let job_id = format!("agent-{}-{}", now_ts(), &digest[..8]);
        let task = Arc::new(ActiveTask {
            cancel: Arc::new(AtomicBool::new(false)),
            child: Arc::new(AsyncMutex::new(None)),
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
                book_url: book.book_url.clone(),
                phase: "loading".to_string(),
                status_text: "准备任务...".to_string(),
                current_chapter_index: None,
                target_chapter_index,
                last_error: None,
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

    pub async fn status(&self) -> AgentTaskSnapshot {
        self.registry
            .lock()
            .await
            .clone()
            .unwrap_or_else(AgentTaskSnapshot::empty)
    }

    pub async fn cancel_task(&self) -> bool {
        let active = self.active.lock().await.clone();
        if let Some(task) = active {
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
            Err(error) => (error.to_string(), Some(error.to_string())),
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
            Err(error) => (error.to_string(), Some(error.to_string())),
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
            .get(user_ns, &book.book_url)
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

        let (child, mut stdin, mut rx) = self.spawn_sidecar().await?;
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
                    .run_frame_until_result(&mut stdin, &mut rx, run_frame, &ctx)
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
                    match self.redraw_map_via_sidecar(&mut stdin, &mut rx, &memory, &model_json).await {
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
            .get(user_ns, &book.book_url)
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

        let (child, mut stdin, mut rx) = self.spawn_sidecar().await?;
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
                .run_frame_until_result(&mut stdin, &mut rx, frame, &ctx)
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
        rx: &mut mpsc::Receiver<String>,
        run_frame: Value,
        ctx: &TaskContext<'_>,
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
                        let content = self.answer_tool_call(ctx, &frame).await;
                        let response = json!({
                            "type": "tool_result",
                            "id": frame.get("id").cloned().unwrap_or(Value::Null),
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
                    Some("result") => return Ok(frame),
                    other => {
                        tracing::warn!(target: SIDECAR_LOG_TARGET, "未知帧类型：{:?}", other);
                    }
                }
            }
            Err(AppError::Internal(anyhow::anyhow!("sidecar 意外退出")))
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
                &book.book_url,
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
        let toc_url = book
            .toc_url
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or(&book.book_url);
        self.book_service
            .get_chapter_list_with_cache(user_ns, &source, &repair_encoded_url(toc_url), false)
            .await
    }

    async fn resolve_source(
        &self,
        user_ns: &str,
        book: &Book,
    ) -> Result<Option<BookSource>, AppError> {
        let origin = normalize_source_url(&book.origin);
        if origin.is_empty() {
            return Ok(None);
        }
        if let Some(source) = self.book_source_service.get(user_ns, &origin).await? {
            return Ok(Some(source));
        }
        self.book_source_service
            .find_by_normalized_url(user_ns, &origin)
            .await
    }

    // -----------------------------------------------------------------------
    // 地图：生成（经 sidecar）+ 落盘（b64 解码或 URL 下载）
    // -----------------------------------------------------------------------

    async fn redraw_map_via_sidecar(
        &self,
        stdin: &mut ChildStdin,
        rx: &mut mpsc::Receiver<String>,
        memory: &AiBookMemory,
        model_json: &Value,
    ) -> Result<MapImage, AppError> {
        let frame = json!({
            "type": "run",
            "job": {
                "jobId": format!("redraw-{}", now_ts()),
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
        let result = self.run_frame_until_result(stdin, rx, frame, &ctx).await?;
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
        self.ai_book_service
            .save_for_book(user_ns, &memory.book_url, memory.clone())
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

    async fn spawn_sidecar(&self) -> Result<(Child, ChildStdin, mpsc::Receiver<String>), AppError> {
        let (mut program, args) = split_command(&self.command)
            .ok_or_else(|| AppError::BadRequest("AGENT_SIDECAR_COMMAND 无法解析".to_string()))?;
        // 开发便利：命令仍是默认值且 sidecar/.venv 存在时，优先用 venv 的 Python——
        // 系统Python 往往没有 sidecar 依赖（ModuleNotFoundError 是最常见握手失败原因）。
        // Docker / 自定义命令不受影响（显式覆盖，见 Dockerfile 的 ENV）。
        if self.command == DEFAULT_SIDECAR_COMMAND {
            if let Some(venv_python) = venv_python(&self.sidecar_dir) {
                tracing::info!(
                    target: SIDECAR_LOG_TARGET,
                    "使用 sidecar venv 的 Python：{}",
                    venv_python.display()
                );
                program = venv_python.to_string_lossy().to_string();
            }
        }
        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .current_dir(&self.sidecar_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn().map_err(|e| {
            AppError::BadRequest(format!(
                "sidecar 启动失败（{program}）：{e}。请确认已安装 Python ≥3.11 并配置 AGENT_SIDECAR_COMMAND"
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

        // stdout 逐行转 mpsc；消费端 drop 后 send 失败即退出
        let (tx, rx) = mpsc::channel::<String>(16);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if tx.send(line).await.is_err() {
                    break;
                }
            }
        });

        // 握手：hello 帧 → 协议版本。失败必须带上子进程 stderr 的真实原因，
        // 否则用户只能看到一句无法定位的 "internal error"
        let mut rx = rx;
        let first_line = match timeout(HANDSHAKE_TIMEOUT, rx.recv()).await {
            Ok(Some(line)) => Some(line),
            Ok(None) => None,
            Err(_) => return Err(handshake_failed(&stderr_tail, "sidecar 握手超时（10 秒）").await),
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
        Ok((child, stdin, rx))
    }
}

/// 地图图片的两种形态：sidecar 返回 b64 或上游 URL。
enum MapImage {
    Bytes(Vec<u8>),
    Url(String),
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
    let client = reqwest::Client::builder()
        .timeout(MAP_IMAGE_DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|e| AppError::Internal(e.into()))?;
    let response = client.get(url).send().await?;
    if let Some(length) = response.content_length() {
        if length > MAX_MAP_IMAGE_BYTES {
            return Err(AppError::BadRequest("地图图片超过大小上限".to_string()));
        }
    }
    let bytes = response.bytes().await?;
    if bytes.len() as u64 > MAX_MAP_IMAGE_BYTES {
        return Err(AppError::BadRequest("地图图片超过大小上限".to_string()));
    }
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

/// 握手失败：附上子进程 stderr 末尾（缺依赖、Store 占位符等真实原因都在那里）。
async fn handshake_failed(stderr_tail: &AsyncMutex<Vec<String>>, reason: &str) -> AppError {
    let tail = stderr_tail.lock().await;
    let detail = if tail.is_empty() {
        String::new()
    } else {
        format!("；子进程 stderr 末尾：{}", tail.join(" ┃ "))
    };
    AppError::BadRequest(format!(
        "{reason}{detail}。请确认：Python ≥3.11 可用（Windows 上 `python` 可能命中          Microsoft Store 占位符）；sidecar 依赖已装进该 Python（pip install httpx==0.28.1          pydantic==2.11.7，或让 AGENT_SIDECAR_COMMAND 指向 sidecar/.venv 里的 python）"
    ))
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
    // 优先工作目录下的 sidecar/（cargo run 在仓库根），其次可执行文件旁（部署态）
    if PathBuf::from("sidecar").join("agent_sidecar").is_dir() {
        return PathBuf::from("sidecar");
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("sidecar");
            if candidate.join("agent_sidecar").is_dir() {
                return candidate;
            }
        }
    }
    PathBuf::from("sidecar")
}
