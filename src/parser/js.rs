use crate::model::book_source::BookSource;
use crate::util::bounded_map::BoundedMap;
use crate::util::hash::md5_hex;
use crate::util::text::{apply_regex_replace, strip_whitespace};
use aes::Aes128;
use base64::Engine;
use cbc::cipher::{block_padding::Pkcs7, BlockModeDecrypt, KeyIvInit};
use chrono::{Local, TimeZone};
use once_cell::sync::Lazy;
use reqwest::blocking::Client;
use reqwest::Method;
use rquickjs::function::Func;
use rquickjs::{Context, Ctx, Object, Runtime, Value};
use serde_json::Value as JsonValue;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// 书源 JS 的全局 KV（`cache.put/get`、`kv_put/get`）。
///
/// 键一律经过 [`scoped_key`] 加用户前缀，避免不同书源/不同用户的键互相覆盖
/// 或读取到他人写入的值。
static JS_KV: Lazy<Mutex<BoundedMap<String>>> =
    Lazy::new(|| Mutex::new(BoundedMap::with_bytes(JS_KV_MAX_ENTRIES, JS_KV_MAX_BYTES)));
/// jsLib 缓存带 TTL：远程 jsLib 原地更新时，无 TTL 的进程内缓存会永久用旧版
const JS_LIB_CACHE_TTL: Duration = Duration::from_secs(3600);
static JS_LIB_CACHE: Lazy<Mutex<BoundedMap<(String, std::time::Instant)>>> =
    Lazy::new(|| Mutex::new(BoundedMap::new(JS_LIB_CACHE_MAX_ENTRIES)));
/// 全局 KV / jsLib 缓存的条目上限。
///
/// 键由书源（第三方内容）决定：一个不断写入新键的书源可以把进程内存吃光，
/// 因此两张表都收进 BoundedMap——超出上限按 FIFO 逐出最旧条目。书源 KV
/// 只用于跨请求缓存，逐出只影响性能，不会破坏正确性。
const JS_KV_MAX_ENTRIES: usize = 4096;
/// KV 总字节上限：value 由书源决定大小，只限条数内存仍可无界增长
const JS_KV_MAX_BYTES: usize = 8 * 1024 * 1024;
const JS_LIB_CACHE_MAX_ENTRIES: usize = 256;
/// 书源 JS 的运行时资源上限。
///
/// 书源内容完全由第三方提供，而 QuickJS 求值是同步的；不设边界时一条
/// `while(true){}` 或一次正则灾难性回溯就能永久占住一个 worker 线程。
const JS_MEMORY_LIMIT_BYTES: usize = 128 * 1024 * 1024;
const JS_MAX_STACK_BYTES: usize = 1024 * 1024;
/// 单次求值的最长时间；由中断处理器打断并抛出不可捕获异常。
/// rquickjs 0.14（QuickJS-NG）下该中断在字节码边界与 libregexp 的执行循环
/// （`lre_poll_timeout`，约每 1 万步轮询一次）都会触发，因此正则灾难性
/// 回溯同样受这 5 秒约束，不会永久占住 worker 线程。
const JS_EVAL_TIMEOUT: Duration = Duration::from_secs(5);
/// 单次**顶层解析**聚合的 JS 执行预算。
///
/// 单次 eval 有 5s 上限，但一次 parse（如万章目录的 formatJs 每章一次、
/// 模板每个一次）里 eval 次数无界——恶意书源可用 4.9s 空转脚本 × 万章目录
/// 占住阻塞线程数小时。解析入口用 [`begin_parse_budget`] 圈定聚合预算，
/// 累计超时后后续 eval 直接拒绝。
const JS_PARSE_BUDGET: Duration = Duration::from_secs(30);
/// 单次求值允许返回的最大字符串长度。
const JS_MAX_RESULT_BYTES: usize = 32 * 1024 * 1024;
/// 慢求值告警阈值：单次求值（含上下文搭建与 jsLib）超过此时长打 WARN。
const JS_SLOW_EVAL_WARN: Duration = Duration::from_millis(1000);

// ─── 进程级求值统计（诊断埋点）───
//
// 全部用 Relaxed 序：只服务可观测性，读取便宜优先于跨线程精确时序。
// 计数器模块私有，对外只暴露 [`js_eval_stats`] 只读快照。
/// 进入求值的总次数（含聚合预算耗尽被拒的调用）。
static JS_EVAL_TOTAL: AtomicU64 = AtomicU64::new(0);
/// 返回 Err 的次数（含预算耗尽与被中断的求值）。
static JS_EVAL_FAILED: AtomicU64 = AtomicU64::new(0);
/// 聚合解析预算耗尽、求值被直接拒绝的次数。
static JS_EVAL_BUDGET_EXHAUSTED: AtomicU64 = AtomicU64::new(0);
/// 被中断处理器打断的次数（单次 5s 超时或聚合预算收紧的 deadline 触发）。
static JS_EVAL_INTERRUPTED: AtomicU64 = AtomicU64::new(0);
/// 累计求值耗时（微秒），用于估算 JS 阶段对 worker 线程的占用。
static JS_EVAL_TOTAL_MICROS: AtomicU64 = AtomicU64::new(0);

/// JS 求值统计快照（进程级，自启动累计；各字段含义见对应计数器）。
#[derive(Debug, Clone, Copy, Default)]
// 消费方（诊断/统计接口）尚未接入，当前由单元测试覆盖；非 test 构建豁免 dead_code。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct JsEvalStats {
    pub total: u64,
    pub failed: u64,
    pub budget_exhausted: u64,
    pub interrupted: u64,
    pub total_micros: u64,
}

/// 读取进程级 JS 求值统计快照。
// 同上：消费方后续接入，先由测试驱动。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn js_eval_stats() -> JsEvalStats {
    JsEvalStats {
        total: JS_EVAL_TOTAL.load(Ordering::Relaxed),
        failed: JS_EVAL_FAILED.load(Ordering::Relaxed),
        budget_exhausted: JS_EVAL_BUDGET_EXHAUSTED.load(Ordering::Relaxed),
        interrupted: JS_EVAL_INTERRUPTED.load(Ordering::Relaxed),
        total_micros: JS_EVAL_TOTAL_MICROS.load(Ordering::Relaxed),
    }
}
/// `java.ajax` 等内部请求的超时（秒）。默认与 `REQUEST_TIMEOUT_SECS` 的代码默认
/// 一致；app 启动时按实际配置覆盖（见 `set_js_http_timeout`）。
/// 注意：JS 求值的 5s 中断只约束纯 JS 执行，原生函数（如 java.ajax）不受其管——
/// 一次求值最坏耗时 ≈ 5s 纯 JS + 每次 HTTP 请求各这么长。
static JS_HTTP_TIMEOUT_SECS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(15);

/// 启动引导按 REQUEST_TIMEOUT_SECS 覆盖 JS 内部请求超时。
pub fn set_js_http_timeout(secs: u64) {
    JS_HTTP_TIMEOUT_SECS.store(secs.max(1), std::sync::atomic::Ordering::Relaxed);
}

fn js_http_timeout() -> Duration {
    Duration::from_secs(JS_HTTP_TIMEOUT_SECS.load(std::sync::atomic::Ordering::Relaxed))
}
/// 书源 JS 内 `java.ajax/get/post/put` 使用的客户端池。
///
/// 同样按用户命名空间隔离：否则 A 用户在站点登录得到的会话 Cookie
/// 会被 B 用户书源的 `java.ajax` 自动带上。
/// 命名空间 client 池上限：BoundedMap 满时按 FIFO 逐出最旧条目（单用户
/// 常态只有 1-2 个条目，上限只是防失控的保险）。旧值 8 在并发活跃命名
/// 空间稍多（代理书源、多标签页并发解析等）时会频繁逐出，`java.ajax`
/// 的站点 Cookie jar 随机丢失；提到 256 后常态远触不到上限。注意 FIFO
/// 逐出不是真正的 LRU（不区分冷热），需要更精细的淘汰策略可后续再做。
const JS_HTTP_CLIENT_MAX: usize = 256;
static JS_HTTP_CLIENTS: Lazy<Mutex<BoundedMap<Client>>> =
    Lazy::new(|| Mutex::new(BoundedMap::new(JS_HTTP_CLIENT_MAX)));
/// 未显式指定命名空间时的归属（封面等匿名请求同此）。
const DEFAULT_USER_NS: &str = "public";
type Aes128CbcDecryptor = cbc::Decryptor<Aes128>;
thread_local! {
    static ACTIVE_JS_LIB: RefCell<Option<String>> = const { RefCell::new(None) };
    /// 每线程一个复用的 QuickJS Runtime：此前每次求值都 `Runtime::new()` +
    /// 注册全部 intrinsics，一个 20 条结果的搜索页就能建上百个 Runtime。
    /// Runtime 不可跨线程（QuickJS 约束），thread_local 是天然池化边界；
    /// 每次求值在其上新建 `Context::full`（全新全局对象，求值间天然隔离）。
    static JS_RUNTIME: RefCell<Option<Runtime>> = const { RefCell::new(None) };
    /// 当前求值所属的用户命名空间。
    static ACTIVE_USER_NS: RefCell<Option<String>> = const { RefCell::new(None) };
    /// 当前书源的 JSON 文本；JS 里的 `source` 就是它（规格 §10）。
    static ACTIVE_SOURCE: RefCell<Option<String>> = const { RefCell::new(None) };
    /// 当前线程上「顶层解析」的聚合 JS 预算：深度计数 + 截止时间。
    /// `None` 表示没有顶层解析在进行（eval 只受单次 5s 上限约束）。
    static PARSE_BUDGET: RefCell<Option<ParseBudgetState>> = const { RefCell::new(None) };
}

/// 聚合 JS 预算的线程内状态。
#[derive(Debug, Clone, Copy)]
struct ParseBudgetState {
    depth: u32,
    deadline: Instant,
}

/// 顶层解析入口的聚合 JS 预算守卫。
///
/// 第一次进入时设 deadline = now + [`JS_PARSE_BUDGET`]；嵌套进入只加深度；
/// drop 时减深度、归零时整表清空（下一次解析重新计时）。解析入口都是同步
/// 函数，guard 只在进入解析的同一线程内生效，不跨 await 持有。
///
/// 预算是 thread_local 状态，守卫跨线程持有会泄漏/错扣别的线程的预算，
/// 因此用 `PhantomData<Cell<()>>` 把它标记成 !Send/!Sync——这是编译期
/// 约束：试图把守卫跨 await 持有（让 future 变成 !Send）或直接发往别的
/// 线程都会直接编译失败，而不是运行期悄悄算错预算。
#[derive(Debug)]
pub struct ParseBudgetGuard {
    /// 仅用于让守卫 !Send/!Sync 的标记字段，不承载数据。
    _not_send: std::marker::PhantomData<std::cell::Cell<()>>,
}

pub fn begin_parse_budget() -> ParseBudgetGuard {
    PARSE_BUDGET.with(|cell| {
        let mut slot = cell.borrow_mut();
        match slot.as_mut() {
            Some(state) => state.depth += 1,
            None => {
                *slot = Some(ParseBudgetState {
                    depth: 1,
                    deadline: Instant::now() + JS_PARSE_BUDGET,
                })
            }
        }
    });
    ParseBudgetGuard {
        _not_send: std::marker::PhantomData,
    }
}

impl Drop for ParseBudgetGuard {
    fn drop(&mut self) {
        PARSE_BUDGET.with(|cell| {
            let mut slot = cell.borrow_mut();
            if let Some(state) = slot.as_mut() {
                state.depth = state.depth.saturating_sub(1);
                if state.depth == 0 {
                    *slot = None;
                }
            }
        });
    }
}

/// 当前线程的聚合预算截止时间（无顶层解析时为 None）。
fn parse_budget_deadline() -> Option<Instant> {
    PARSE_BUDGET.with(|cell| cell.borrow().as_ref().map(|state| state.deadline))
}

/// 覆盖当前聚合预算的时长（仅测试用；预算不存在时 no-op）。
#[cfg(test)]
pub(crate) fn set_parse_budget_for_test(budget: Duration) {
    PARSE_BUDGET.with(|cell| {
        if let Some(state) = cell.borrow_mut().as_mut() {
            state.deadline = Instant::now() + budget;
        }
    });
}

pub fn with_js_lib<T>(js_lib: Option<&str>, f: impl FnOnce() -> T) -> T {
    let _guard = swap_tl(&ACTIVE_JS_LIB, js_lib.map(|value| value.to_string()));
    f()
}

/// 在「书源上下文」里执行闭包：顺带挂上 jsLib，并让 JS 里的 `source` 拿到这个
/// 书源的真实字段（`bookSourceUrl`、`bookSourceName`、`header`…）。
///
/// 书源规则经常写 `{{source.bookSourceUrl}}` 拼 URL，只给一个空壳会让这类规则
/// 静默取到空串。注意必须在**执行求值的那个线程**上调用（解析跑在阻塞线程池里，
/// 在 async 侧设置是无效的）。
pub fn with_book_source<T>(source: &BookSource, f: impl FnOnce() -> T) -> T {
    let json = serde_json::to_string(source).ok();
    let _source_guard = swap_tl(&ACTIVE_SOURCE, json);
    with_js_lib(source.js_lib.as_deref(), f)
}

fn active_source_json() -> Option<String> {
    ACTIVE_SOURCE.with(|cell| cell.borrow().clone())
}

/// 从 `ACTIVE_SOURCE` 提取书源名/URL 用于日志上下文；读不到返回 `(None, None)`。
///
/// 仅在告警等冷路径调用（有一次 JSON 解析成本），不放求值热路径。
fn active_source_identity() -> (Option<String>, Option<String>) {
    let extract = || {
        let json = ACTIVE_SOURCE.with(|cell| cell.borrow().clone())?;
        let value = serde_json::from_str::<JsonValue>(&json).ok()?;
        let name = value
            .get("bookSourceName")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let url = value
            .get("bookSourceUrl")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        Some((name, url))
    };
    extract().unwrap_or((None, None))
}

/// 在指定用户命名空间下执行闭包。
///
/// 书源 JS 的 `cache`/`kv`/`java.*` 都会读取当前命名空间做隔离；
/// 未设置时归入 [`DEFAULT_USER_NS`]，绝不与其他用户共享。
pub fn with_user_ns<T>(user_ns: &str, f: impl FnOnce() -> T) -> T {
    let _guard = swap_tl(&ACTIVE_USER_NS, Some(user_ns.to_string()));
    f()
}

/// thread_local 的换值-恢复守卫。此前是手写 `replace → f() → replace`：
/// 闭包 panic 时恢复那一步被跳过，旧值（比如上个用户的 ns）残留在阻塞线程上，
/// 下一个复用该线程的请求就会读到串号上下文。Drop 在 panic 展开时同样执行。
struct TlRestoreGuard<T: 'static> {
    key: &'static std::thread::LocalKey<RefCell<Option<T>>>,
    previous: Option<T>,
}

impl<T: 'static> Drop for TlRestoreGuard<T> {
    fn drop(&mut self) {
        let previous = self.previous.take();
        self.key.with(|cell| {
            cell.replace(previous);
        });
    }
}

fn swap_tl<T: 'static>(
    key: &'static std::thread::LocalKey<RefCell<Option<T>>>,
    value: Option<T>,
) -> TlRestoreGuard<T> {
    let previous = key.with(|cell| cell.replace(value));
    TlRestoreGuard { key, previous }
}

fn current_user_ns() -> String {
    ACTIVE_USER_NS
        .with(|cell| cell.borrow().clone())
        .unwrap_or_else(|| DEFAULT_USER_NS.to_string())
}

/// 给 KV 键加上命名空间前缀（用 Unit Separator 分隔，避免与业务键混淆）。
fn scoped_key(key: &str) -> String {
    format!("{}\u{1f}{}", current_user_ns(), key)
}

fn kv_get_scoped(key: &str) -> Option<String> {
    let map = JS_KV.lock().unwrap_or_else(|e| e.into_inner());
    map.get(&scoped_key(key)).cloned()
}

fn kv_put_scoped(key: &str, value: &str) {
    let scoped = scoped_key(key);
    JS_KV
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert_sized(scoped, value.to_string());
}

/// 每个命名空间一个稳定的设备标识（模拟阅读 App 的 deviceID）。
fn device_id() -> String {
    const DEVICE_KEY: &str = "__device_id";
    if let Some(existing) = kv_get_scoped(DEVICE_KEY) {
        return existing;
    }
    let generated = Uuid::new_v4().to_string();
    kv_put_scoped(DEVICE_KEY, &generated);
    generated
}

/// 取当前命名空间的 JS HTTP 客户端（独立 Cookie jar）。
fn js_http_client() -> Client {
    let ns = current_user_ns();
    if let Ok(map) = JS_HTTP_CLIENTS.lock() {
        if let Some(client) = map.get(&ns) {
            return client.clone();
        }
    }
    // BoundedMap 满时 FIFO 逐出最旧命名空间的 client（其 Cookie jar 一并失效）
    let client = Client::builder()
        .cookie_store(true)
        .timeout(js_http_timeout())
        .gzip(true)
        .brotli(true)
        .deflate(true)
        // 与全局一致的重定向策略：限制跳数，开启防护时拒绝跳往内网
        .redirect(crate::crawler::url_guard::guarded_redirect_policy())
        // 解析期守卫：堵住「预检解析一次、连接再解析一次」的 DNS rebinding 窗口
        .dns_resolver(crate::crawler::url_guard::guarded_dns_resolver())
        .build()
        .expect("failed to build JS HTTP client");
    let mut map = JS_HTTP_CLIENTS.lock().unwrap_or_else(|e| e.into_inner());
    map.insert(ns, client.clone());
    client
}

pub fn eval_js(script: &str, input: &str, base_url: &str) -> anyhow::Result<String> {
    Ok(eval_js_inner(script, Some(input), Some(base_url), None, None, None)?.0)
}

pub fn eval_js_with_bindings(
    script: &str,
    input: &str,
    base_url: &str,
    bindings: &HashMap<String, JsonValue>,
) -> anyhow::Result<String> {
    Ok(eval_js_inner(
        script,
        Some(input),
        Some(base_url),
        None,
        None,
        Some(bindings),
    )?
    .0)
}

/// 与 [`eval_js_with_bindings`] 相同，但额外回读脚本执行后的全局变量。
///
/// `formatJs` 的 `gInt` 需要在同一轮格式化的多章之间传递，而每次求值都是全新的
/// Runtime，只能由调用方把值带回来。
pub fn eval_js_with_bindings_and_globals(
    script: &str,
    input: &str,
    base_url: &str,
    bindings: &HashMap<String, JsonValue>,
    read_globals: &[&str],
) -> anyhow::Result<(String, HashMap<String, JsonValue>)> {
    eval_js_inner_with_source(
        script,
        Some(input),
        Some(base_url),
        None,
        None,
        None,
        Some(bindings),
        Some(read_globals),
    )
}

pub fn eval_js_url(
    script: &str,
    result: &str,
    key: &str,
    page: i32,
    source_key: &str,
    base_url: &str,
) -> anyhow::Result<String> {
    Ok(eval_js_inner_with_source(
        script,
        Some(result),
        Some(base_url),
        Some(key),
        Some(page),
        Some(source_key),
        None,
        None,
    )?
    .0)
}

fn eval_js_inner(
    script: &str,
    input: Option<&str>,
    base_url: Option<&str>,
    key: Option<&str>,
    page: Option<i32>,
    bindings: Option<&HashMap<String, JsonValue>>,
) -> anyhow::Result<(String, HashMap<String, JsonValue>)> {
    eval_js_inner_with_source(script, input, base_url, key, page, None, bindings, None)
}

#[allow(clippy::too_many_arguments)]
fn eval_js_inner_with_source(
    script: &str,
    input: Option<&str>,
    base_url: Option<&str>,
    key: Option<&str>,
    page: Option<i32>,
    source_key: Option<&str>,
    bindings: Option<&HashMap<String, JsonValue>>,
    read_globals: Option<&[&str]>,
) -> anyhow::Result<(String, HashMap<String, JsonValue>)> {
    let started = Instant::now();
    JS_EVAL_TOTAL.fetch_add(1, Ordering::Relaxed);
    // 聚合预算：进入时已超时直接拒绝执行（恶意书源用大量近满额 eval 拖垮
    // 阻塞线程的场景在此截断）；未超时则把本次求值的中断 deadline 收紧到
    // min(单次 5s, 聚合截止)
    let parse_deadline = parse_budget_deadline();
    if parse_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        JS_EVAL_BUDGET_EXHAUSTED.fetch_add(1, Ordering::Relaxed);
        let (source_name, source_url) = active_source_identity();
        tracing::warn!(
            budget_secs = JS_PARSE_BUDGET.as_secs(),
            script_len = script.len(),
            source_name = ?source_name,
            source_url = ?source_url,
            "JS 聚合解析预算耗尽，拒绝求值"
        );
        let result = Err(anyhow::anyhow!(
            "解析预算耗尽：单次解析聚合 JS 执行超过 {} 秒",
            JS_PARSE_BUDGET.as_secs()
        ));
        record_eval_outcome(started, script.len(), bindings.is_some(), &result);
        return result;
    }
    let result = JS_RUNTIME.with(|cell| {
        {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                let rt = Runtime::new()?;
                rt.set_memory_limit(JS_MEMORY_LIMIT_BYTES);
                rt.set_max_stack_size(JS_MAX_STACK_BYTES);
                *slot = Some(rt);
            }
        }
        let rt_ref = cell.borrow();
        let rt = rt_ref.as_ref().expect("runtime just initialized");
        // 资源上限幂等重设 + 每次求值重挂中断（deadline 是每次新的）
        rt.set_memory_limit(JS_MEMORY_LIMIT_BYTES);
        rt.set_max_stack_size(JS_MAX_STACK_BYTES);
        let deadline = match parse_deadline {
            Some(parse_deadline) => parse_deadline.min(Instant::now() + JS_EVAL_TIMEOUT),
            None => Instant::now() + JS_EVAL_TIMEOUT,
        };
        rt.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
        let ctx = Context::full(rt)?;
        ctx.with(|ctx| {
            let globals = ctx.globals();
            let input_value = input.unwrap_or("");
            let base_url_value = base_url.unwrap_or("");
            let shared_js = active_js_lib_script()?;

            globals.set("input", input_value)?;
            globals.set("result", input_value)?;
            globals.set("src", input_value)?;
            globals.set("base_url", base_url_value)?;
            globals.set("baseUrl", base_url_value)?;
            if let Some(key) = key {
                globals.set("key", key)?;
            }
            if let Some(page) = page {
                globals.set("page", page)?;
            }

            // Default url variable for Legado compatibility
            globals.set("url", base_url_value)?;

            // Stubs for Legado compatibility
            // 书源对象：有 `with_book_source` 上下文时是真实书源，否则退化成只带 key 的空壳。
            match active_source_json().and_then(|json| ctx.json_parse(json).ok()) {
                Some(value) => globals.set("source", value)?,
                None => globals.set("source", Object::new(ctx.clone())?)?,
            }
            let source_obj: Object = globals.get("source")?;
            // `key`/`getKey()` 是既有的兼容绑定：优先用调用方给的（URL 规则会传书源 URL），
            // 否则回退到书源自己的 `bookSourceUrl`。
            let source_key_val = match source_key {
                Some(key) => key.to_string(),
                None => source_obj
                    .get::<_, Option<String>>("bookSourceUrl")
                    .unwrap_or_default()
                    .unwrap_or_default(),
            };
            let sk_clone = source_key_val.clone();
            source_obj.set("key", source_key_val)?;
            source_obj.set("getKey", Func::new(move || sk_clone.clone()))?;

            let cookie_obj = Object::new(ctx.clone())?;
            cookie_obj.set(
                "removeCookie",
                Func::new(|_key: String| -> String { "".to_string() }),
            )?;
            globals.set("cookie", cookie_obj)?;

            let cache_obj = Object::new(ctx.clone())?;
            cache_obj.set(
                "get",
                Func::new(|key: String| -> Option<String> { kv_get_scoped(&key) }),
            )?;
            cache_obj.set(
                "put",
                Func::new(|key: String, val: String| -> bool {
                    kv_put_scoped(&key, &val);
                    true
                }),
            )?;
            globals.set("cache", cache_obj)?;

            let java_obj = Object::new(ctx.clone())?;
            java_obj.set(
                "ajax",
                Func::new(|spec: String| -> String { java_ajax(&spec).unwrap_or_default() }),
            )?;
            java_obj.set(
                "md5Encode",
                Func::new(|input: String| -> String { md5_hex(&input) }),
            )?;
            java_obj.set(
                "timeFormat",
                Func::new(|timestamp: i64| -> String { java_time_format(timestamp) }),
            )?;
            java_obj.set("androidId", Func::new(|| -> String { device_id() }))?;
            java_obj.set("deviceID", Func::new(|| -> String { device_id() }))?;
            java_obj.set(
                "get",
                Func::new(|url: String| -> String {
                    java_request_simple("GET", &url, None).unwrap_or_default()
                }),
            )?;
            java_obj.set(
                "post",
                Func::new(|url: String, body: String| -> String {
                    java_request_simple("POST", &url, Some(body)).unwrap_or_default()
                }),
            )?;
            java_obj.set(
                "put",
                Func::new(|url: String, body: String| -> String {
                    java_request_simple("PUT", &url, Some(body)).unwrap_or_default()
                }),
            )?;
            java_obj.set(
                "base64Encode",
                Func::new(|input: String| -> String {
                    base64::engine::general_purpose::STANDARD.encode(input)
                }),
            )?;
            java_obj.set(
                "base64Decode",
                Func::new(|input: String| -> String {
                    base64::engine::general_purpose::STANDARD
                        .decode(input)
                        .ok()
                        .and_then(|bytes| String::from_utf8(bytes).ok())
                        .unwrap_or_default()
                }),
            )?;
            java_obj.set(
                "aesBase64DecodeToString",
                Func::new(
                    |input: String, key: String, algorithm: String, iv: String| -> String {
                        java_aes_base64_decode_to_string(&input, &key, &algorithm, &iv)
                    },
                ),
            )?;
            java_obj.set(
                "encodeURIComponent",
                Func::new(|input: String| -> String { js_encode_uri_component(&input) }),
            )?;
            java_obj.set(
                "decodeURIComponent",
                Func::new(|input: String| -> String { js_decode_uri_component(&input) }),
            )?;
            java_obj.set(
                "encodeURI",
                Func::new(|input: String| -> String { js_encode_uri(&input) }),
            )?;
            java_obj.set(
                "decodeURI",
                Func::new(|input: String| -> String { js_decode_uri(&input) }),
            )?;
            java_obj.set(
                "now",
                Func::new(|| -> i64 { chrono::Utc::now().timestamp_millis() }),
            )?;
            java_obj.set(
                "uuid",
                Func::new(|| -> String { Uuid::new_v4().to_string() }),
            )?;
            // 通知/UI 类方法在服务端没有对应动作。注册成空操作而不是留空，
            // 否则书源里一句 `java.log(...)` 就会抛 TypeError、让整条规则失效
            // （规格 §10 允许 UI 类函数实现为空操作，但要求文档声明）。
            java_obj.set("log", Func::new(|_message: String| {}))?;
            java_obj.set("toast", Func::new(|_message: String| {}))?;
            java_obj.set("openUrl", Func::new(|_url: String| {}))?;
            globals.set("java", java_obj)?;

            globals.set(
                "kv_get",
                Func::new(|key: String| -> Option<String> { kv_get_scoped(&key) }),
            )?;
            globals.set(
                "kv_put",
                Func::new(|key: String, val: String| -> bool {
                    kv_put_scoped(&key, &val);
                    true
                }),
            )?;
            globals.set(
                "regex_replace",
                Func::new(
                    |input: String, pattern: String, replace: String| -> String {
                        apply_regex_replace(&input, &pattern, &replace)
                    },
                ),
            )?;
            globals.set(
                "strip_ws",
                Func::new(|input: String| -> String { strip_whitespace(&input) }),
            )?;

            globals.set("book", Object::new(ctx.clone())?)?;
            globals.set("chapter", Object::new(ctx.clone())?)?;
            globals.set("title", "")?;
            globals.set("nextChapterUrl", "")?;
            globals.set("rssArticle", Object::new(ctx.clone())?)?;

            if let Some(bindings) = bindings {
                for (key, value) in bindings {
                    let js_value = ctx.json_parse(value.to_string())?;
                    globals.set(key.as_str(), js_value)?;
                }
            }

            if !shared_js.trim().is_empty() {
                eval_script(ctx.clone(), &shared_js)?;
            }

            let v = eval_script(ctx.clone(), script)?;

            let result = if v.is_null() || v.is_undefined() {
                String::new()
            } else if let Some(s) = v.clone().into_string() {
                let s: rquickjs::String<'_> = s;
                s.to_string()
                    .map(|value| value.to_string())
                    .unwrap_or_default()
            } else {
                match ctx.json_stringify(v) {
                    Ok(Some(json)) => json.to_string().unwrap_or_default(),
                    _ => String::new(),
                }
            };
            if result.len() > JS_MAX_RESULT_BYTES {
                anyhow::bail!("JS 返回结果过大: {} 字节", result.len());
            }

            let mut globals_out = HashMap::new();
            if let Some(names) = read_globals {
                for name in names {
                    if let Some(value) = global_to_json(&ctx, name) {
                        globals_out.insert((*name).to_string(), value);
                    }
                }
            }

            Ok((result, globals_out))
        })
    });
    record_eval_outcome(started, script.len(), bindings.is_some(), &result);
    result
}

/// 求值出口的统一埋点：累计耗时、失败/中断计数、debug 耗时日志与慢求值告警。
///
/// 脚本内容一律不进日志（书源规则可能夹带站点 Cookie 等敏感片段），只记长度。
fn record_eval_outcome(
    started: Instant,
    script_len: usize,
    has_bindings: bool,
    result: &anyhow::Result<(String, HashMap<String, JsonValue>)>,
) {
    let elapsed = started.elapsed();
    JS_EVAL_TOTAL_MICROS.fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
    let success = result.is_ok();
    let mut interrupted = false;
    if let Err(err) = result {
        JS_EVAL_FAILED.fetch_add(1, Ordering::Relaxed);
        interrupted = is_interrupted_error(err);
        if interrupted {
            JS_EVAL_INTERRUPTED.fetch_add(1, Ordering::Relaxed);
        }
    }
    // 结构化字段由 tracing 惰性求值：debug 未启用时下面的表达式不会执行。
    tracing::debug!(
        elapsed_ms = elapsed.as_millis() as u64,
        script_len,
        has_bindings,
        success,
        interrupted,
        "JS 求值完成"
    );
    if elapsed >= JS_SLOW_EVAL_WARN {
        // 书源上下文的 JSON 解析有成本，只放在告警（冷）路径上，不进热路径。
        let (source_name, source_url) = active_source_identity();
        tracing::warn!(
            elapsed_ms = elapsed.as_millis() as u64,
            script_len,
            has_bindings,
            success,
            source_name = ?source_name,
            source_url = ?source_url,
            "JS 慢求值：单次耗时超过 {}ms",
            JS_SLOW_EVAL_WARN.as_millis()
        );
    }
}

/// 识别「被中断处理器打断」的求值失败：QuickJS-NG 在中断 handler 返回 true 时
/// 抛出消息为 "interrupted" 的 InternalError。脚本自行 throw 含同字样的错误会
/// 被误计——诊断计数取近似即可，不影响求值行为。
fn is_interrupted_error(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.to_string().contains("interrupted"))
}

/// 把脚本执行后的 JS 全局变量读成 JSON 值；不支持的类型（对象/函数等）返回 `None`。
fn global_to_json(ctx: &Ctx<'_>, name: &str) -> Option<JsonValue> {
    let value: Value = ctx.globals().get(name).ok()?;
    if let Some(int) = value.as_int() {
        return Some(JsonValue::from(int));
    }
    if let Some(float) = value.as_float() {
        return Some(JsonValue::from(float));
    }
    if let Some(flag) = value.as_bool() {
        return Some(JsonValue::from(flag));
    }
    let text = value.into_string()?.to_string().ok()?;
    Some(JsonValue::from(text))
}

fn java_aes_base64_decode_to_string(input: &str, key: &str, algorithm: &str, iv: &str) -> String {
    let algorithm = algorithm.to_ascii_uppercase();
    if algorithm != "AES/CBC/PKCS5PADDING" && algorithm != "AES/CBC/PKCS7PADDING" {
        return String::new();
    }

    let Ok(mut encrypted) = base64::engine::general_purpose::STANDARD.decode(input.trim()) else {
        return String::new();
    };

    let Ok(cipher) = Aes128CbcDecryptor::new_from_slices(key.as_bytes(), iv.as_bytes()) else {
        return String::new();
    };

    cipher
        .decrypt_padded::<Pkcs7>(&mut encrypted)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
        .unwrap_or_default()
}

/// ECMAScript 风格百分号编码：`keep` 命中的字节原样保留，其余按 UTF-8 字节输出大写 %XX。
fn percent_encode_keep(input: &str, keep: fn(u8) -> bool) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for &byte in input.as_bytes() {
        if keep(byte) {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0xF) as usize] as char);
        }
    }
    out
}

fn is_uri_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
        )
}

fn is_uri_reserved(byte: u8) -> bool {
    matches!(
        byte,
        b';' | b'/' | b'?' | b':' | b'@' | b'&' | b'=' | b'+' | b'$' | b',' | b'#'
    )
}

/// `encodeURIComponent`：只保留非保留字符（A-Z a-z 0-9 与 - _ . ! ~ * ' ( )）。
fn js_encode_uri_component(input: &str) -> String {
    percent_encode_keep(input, is_uri_unreserved)
}

/// `encodeURI`：额外保留 URI 保留字符 ; / ? : @ & = + $ , #，可直接处理完整 URL。
fn js_encode_uri(input: &str) -> String {
    percent_encode_keep(input, |byte| {
        is_uri_unreserved(byte) || is_uri_reserved(byte)
    })
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// ECMAScript 风格百分号解码。`keep_reserved`（decodeURI）时，解码结果是
/// URI 保留字符的 %XX 序列保持原样；非法序列一律按原文保留而不是清空整串。
fn percent_decode(input: &str, keep_reserved: bool) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                let decoded = (hi << 4) | lo;
                if keep_reserved && is_uri_reserved(decoded) {
                    out.extend_from_slice(&bytes[i..i + 3]);
                } else {
                    out.push(decoded);
                }
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn js_decode_uri(input: &str) -> String {
    percent_decode(input, true)
}

fn js_decode_uri_component(input: &str) -> String {
    percent_decode(input, false)
}

fn eval_script<'js>(ctx: rquickjs::Ctx<'js>, script: &str) -> anyhow::Result<Value<'js>> {
    let first = match ctx.eval(script) {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };

    let exception = ctx.catch().into_exception();
    let message = exception
        .as_ref()
        .map(|exception| format!("{exception:?}"))
        .unwrap_or_else(|| first.to_string());

    // 书源常写 `@js:return ...`，而脚本语义下顶层 `return` 是语法错误
    // （`return not in a function`）。只对这一种错误兜底：把规则包成 IIFE 再求值，
    // 表达式风格的规则不受影响。
    if message.contains("return not in a function") {
        let wrapped = format!("(function(){{{script}\n}})()");
        if let Ok(value) = ctx.eval(wrapped) {
            return Ok(value);
        }
    }

    match exception {
        Some(exception) => Err(anyhow::anyhow!("JS Exception: {:?}", exception)),
        None => Err(first.into()),
    }
}

fn active_js_lib_script() -> anyhow::Result<String> {
    let js_lib = ACTIVE_JS_LIB.with(|cell| cell.borrow().clone());
    let Some(js_lib) = js_lib.filter(|value| !value.trim().is_empty()) else {
        return Ok(String::new());
    };
    let cache_key = md5_hex(&js_lib);
    {
        let mut cache = JS_LIB_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((script, cached_at)) = cache.get(&cache_key) {
            if cached_at.elapsed() < JS_LIB_CACHE_TTL {
                return Ok(script.clone());
            }
            // 过期条目摘掉，重新编译（远程 jsLib 原地更新的场景）
            let key = cache_key.clone();
            cache.remove(&key);
        }
    }

    let compiled = compile_js_lib(&js_lib)?;
    {
        JS_LIB_CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(cache_key, (compiled.clone(), std::time::Instant::now()));
    }
    Ok(compiled)
}

fn compile_js_lib(js_lib: &str) -> anyhow::Result<String> {
    let trimmed = js_lib.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    if trimmed.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<JsonValue>(trimmed) {
            if let Some(map) = value.as_object() {
                let mut scripts = Vec::new();
                for entry in map.values() {
                    if let Some(raw) = entry.as_str() {
                        scripts.push(resolve_js_lib_entry(raw)?);
                    }
                }
                return Ok(scripts.join("\n"));
            }
        }
    }
    Ok(trimmed.to_string())
}

fn resolve_js_lib_entry(entry: &str) -> anyhow::Result<String> {
    let value = entry.trim();
    if value.starts_with("http://") || value.starts_with("https://") {
        // 出站守卫：书源 jsLib 可以任意指定 URL，不校验即是盲打内网的 SSRF 通道
        crate::crawler::url_guard::ensure_outbound_url_str_allowed_blocking(value)
            .map_err(|reason| anyhow::anyhow!("jsLib 远程拉取被出站策略拒绝: {reason}"))?;
        let response = js_http_client().get(value).send()?;
        return read_blocking_body_limited(response);
    }
    Ok(value.to_string())
}

/// JS 侧 HTTP 响应体的限量读取：JS 堆 128MB 上限管不到 Rust 侧的响应缓冲，
/// 无界 text() 会让恶意书源用超大响应直接耗尽进程内存。
fn read_blocking_body_limited(response: reqwest::blocking::Response) -> anyhow::Result<String> {
    use std::io::Read;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let limit = crate::crawler::fetcher::MAX_RESPONSE_BYTES;
    let mut buf = Vec::new();
    response.take(limit + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > limit {
        anyhow::bail!("JS HTTP 响应体超过 {limit} 字节上限");
    }
    Ok(crate::crawler::fetcher::decode_body(
        &buf,
        None,
        content_type.as_deref(),
    ))
}

fn java_time_format(timestamp: i64) -> String {
    let secs = if timestamp > 1_000_000_000_000 {
        timestamp / 1000
    } else {
        timestamp
    };
    match Local.timestamp_opt(secs, 0).single() {
        Some(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        None => String::new(),
    }
}

fn java_ajax(spec: &str) -> anyhow::Result<String> {
    let (url, options) = split_ajax_spec(spec);
    if url.trim().is_empty() {
        return Ok(String::new());
    }
    // 出站守卫：书源脚本可以任意指定 URL
    if crate::crawler::url_guard::ensure_outbound_url_str_allowed_blocking(url).is_err() {
        return Ok(String::new());
    }

    let options_json = options
        .and_then(|raw| serde_json::from_str::<JsonValue>(raw).ok())
        .unwrap_or(JsonValue::Null);

    let method = options_json
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET")
        .to_uppercase();
    let method = Method::from_bytes(method.as_bytes()).unwrap_or(Method::GET);

    let mut req = js_http_client().request(method, url.trim());

    if let Some(headers) = options_json.get("headers").and_then(|v| v.as_object()) {
        for (key, value) in headers {
            if let Some(value) = value.as_str() {
                req = req.header(key, value);
            } else if !value.is_null() {
                req = req.header(key, value.to_string());
            }
        }
    }

    if let Some(body) = options_json.get("body") {
        if let Some(body) = body.as_str() {
            req = req.body(body.to_string());
        } else if !body.is_null() {
            req = req.body(body.to_string());
        }
    }

    let response = req.send()?;
    read_blocking_body_limited(response)
}

fn java_request_simple(method: &str, url: &str, body: Option<String>) -> anyhow::Result<String> {
    // 出站守卫：书源脚本可以任意指定 URL
    if crate::crawler::url_guard::ensure_outbound_url_str_allowed_blocking(url).is_err() {
        return Ok(String::new());
    }
    let method = Method::from_bytes(method.as_bytes()).unwrap_or(Method::GET);
    let mut req = js_http_client().request(method, url.trim());
    if let Some(body) = body {
        req = req.body(body);
    }
    let response = req.send()?;
    read_blocking_body_limited(response)
}

fn split_ajax_spec(spec: &str) -> (&str, Option<&str>) {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut quote = '\0';
    let mut escaped = false;

    for (idx, ch) in spec.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }

        match ch {
            '\\' if in_string => {
                escaped = true;
            }
            '"' | '\'' if in_string && ch == quote => {
                in_string = false;
                quote = '\0';
            }
            '"' | '\'' if !in_string => {
                in_string = true;
                quote = ch;
            }
            '{' | '[' if !in_string => depth += 1,
            '}' | ']' if !in_string => depth -= 1,
            ',' if !in_string && depth == 0 => {
                let left = &spec[..idx];
                let right = &spec[idx + ch.len_utf8()..];
                return (left, Some(right.trim()));
            }
            _ => {}
        }
    }

    (spec, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// 带看门狗的求值。JS 沙箱若失去超时能力，测试会失败而不是永久挂死整个
    /// `cargo test`（泄漏一个线程，但能拿到明确结论）。
    fn eval_watched(script: &str, wait: Duration) -> Option<anyhow::Result<String>> {
        let (tx, rx) = mpsc::channel();
        let script = script.to_string();
        std::thread::spawn(move || {
            let _ = tx.send(eval_js(&script, "", ""));
        });
        rx.recv_timeout(wait).ok()
    }

    #[test]
    fn normal_js_still_works() {
        assert_eq!(
            eval_js("'hello' + ' ' + 'world'", "", "").unwrap(),
            "hello world"
        );
        // 字符串方法 / 正则等常用能力不受资源上限影响
        assert_eq!(eval_js("'a,b,c'.split(',').length", "", "").unwrap(), "3");
    }

    #[test]
    fn thread_local_context_restores_after_closure_panic() {
        // 闭包 panic 时旧值也要恢复（unwind 走 Drop），不能把上个用户的
        // 命名空间残留在阻塞线程上让下一个请求串号
        let _ = std::panic::catch_unwind(|| {
            with_user_ns("user_victim", || panic!("boom"));
        });
        assert_eq!(current_user_ns(), DEFAULT_USER_NS);
    }

    #[test]
    fn java_uri_codecs_follow_ecmascript_semantics() {
        // encodeURIComponent：URI 保留字符一律编码
        assert_eq!(
            eval_js("java.encodeURIComponent('a b?c=d&e')", "", "").unwrap(),
            "a%20b%3Fc%3Dd%26e"
        );
        // encodeURI：保留字符原样保留，中文等非 URL 字符照常编码
        assert_eq!(
            eval_js(
                "java.encodeURI('https://x.com/p?q=中文&page=1#top')",
                "",
                ""
            )
            .unwrap(),
            "https://x.com/p?q=%E4%B8%AD%E6%96%87&page=1#top"
        );
        // decodeURIComponent：全部 %XX 解码
        assert_eq!(
            eval_js("java.decodeURIComponent('a%20b%3Fc%3Dd')", "", "").unwrap(),
            "a b?c=d"
        );
        // decodeURI：解码结果是保留字符的序列保持原样，其余照解
        assert_eq!(
            eval_js("java.decodeURI('https%3A//x.com/%E4%B8%AD%20q')", "", "").unwrap(),
            "https%3A//x.com/中 q"
        );
    }

    #[test]
    fn notification_helpers_are_registered_as_noops() {
        assert_eq!(
            eval_js(
                "java.log('a'), java.toast('b'), java.openUrl('c'), 'ok'",
                "",
                ""
            )
            .unwrap(),
            "ok"
        );
    }

    #[test]
    fn top_level_return_is_wrapped_into_a_function() {
        // 书源常用 `@js:return ...`，脚本语义下顶层 return 是语法错误，这里兜底包成 IIFE
        assert_eq!(eval_js("return 'ok'", "", "").unwrap(), "ok");
        assert_eq!(
            eval_js("var n = 2; return `${n}-ok`", "", "").unwrap(),
            "2-ok"
        );
        // 表达式风格不受影响
        assert_eq!(eval_js("'ok'", "", "").unwrap(), "ok");
        // 其它语法错误仍然报错
        assert!(eval_js("return )", "", "").is_err());
    }

    #[test]
    fn source_object_exposes_real_book_source_fields() {
        let source = BookSource {
            book_source_name: "测试源".to_string(),
            book_source_url: "https://source.example".to_string(),
            ..Default::default()
        };

        let value = with_book_source(&source, || {
            eval_js(
                "source.bookSourceUrl + '|' + source.bookSourceName + '|' + source.getKey()",
                "",
                "",
            )
        })
        .unwrap();

        assert_eq!(
            value,
            "https://source.example|测试源|https://source.example"
        );

        // 没有书源上下文时仍是空壳，不会 panic
        assert_eq!(
            eval_js("source.bookSourceUrl || 'empty'", "", "").unwrap(),
            "empty"
        );
    }

    #[test]
    fn globals_can_be_read_back_after_eval() {
        let mut bindings = HashMap::new();
        bindings.insert("gInt".to_string(), JsonValue::from(0));

        let (result, globals) =
            eval_js_with_bindings_and_globals("`${gInt++}-x`", "", "", &bindings, &["gInt"])
                .unwrap();

        assert_eq!(result, "0-x");
        // QuickJS 把自增结果表示为浮点，读取方要同时接受整数与浮点
        assert_eq!(globals.get("gInt"), Some(&JsonValue::from(1.0)));
    }

    #[test]
    fn infinite_loop_is_interrupted_not_hung() {
        // 书源里的 while(true){} 必须被中断处理器在超时后打断
        match eval_watched("while(true){}", Duration::from_secs(20)) {
            None => panic!("JS 死循环未被中断处理器打断：20s 内未返回"),
            Some(Ok(v)) => panic!("JS 死循环竟然返回成功: {v:?}"),
            Some(Err(_)) => {}
        }
    }

    #[test]
    fn js_regex_backtracking_is_interrupted() {
        // 灾难性回溯：/(a+)+$ 对「40 个 a + 结尾非 a 字符」要穷举 2^39 种分组，
        // 纯回溯引擎永远跑不完。rquickjs 0.7（Bellard QuickJS）的 libregexp
        // 执行循环不检查中断标志，这类脚本会永久占住一个 spawn_blocking 线程，
        // 内存上限也管不住（耗时间不分配）；rquickjs 0.9+ 换用 QuickJS-NG，
        // libregexp 每约 1 万步轮询一次 lre_check_timeout → 中断处理器可以打断。
        // 注意负载必须「不匹配」（结尾加 b）：纯 'a' 串会被首次贪婪匹配直接吃掉，
        // 不产生回溯。
        // 先自证正则本身合法且引擎对「可匹配」输入立即成功：防止把正则语法
        // 错误之类的即时 Err 误当「成功中断」的假阳性。
        assert_eq!(
            eval_js("new RegExp('(a+)+$').test('a'.repeat(40))", "", "").unwrap(),
            "true"
        );
        let script = "new RegExp('(a+)+$').test('a'.repeat(40) + 'b')";
        match eval_watched(script, Duration::from_secs(10)) {
            None => panic!("正则灾难性回溯未被中断处理器打断：10s 内未返回"),
            Some(Ok(v)) => panic!("指数回溯竟然正常返回: {v:?}（中断未生效）"),
            Some(Err(_)) => {}
        }
    }

    #[test]
    fn unbounded_allocation_is_rejected() {
        // 持续分配必须撞上 128MB 内存上限（或先撞 5s 超时），而不是打爆进程
        let script = "var a=[]; while(true){ a.push(new Array(65536).fill(1)); }";
        match eval_watched(script, Duration::from_secs(30)) {
            None => panic!("JS 无限分配未被内存上限或超时拦住：30s 内未返回"),
            Some(Ok(v)) => panic!("JS 无限分配竟然返回成功: {v:?}"),
            Some(Err(_)) => {}
        }
    }

    #[test]
    fn kv_is_isolated_between_users() {
        with_user_ns("alice", || kv_put_scoped("shared", "alice-value"));
        // bob 既读不到 alice 的值，也覆盖不了它
        with_user_ns("bob", || {
            assert_eq!(kv_get_scoped("shared"), None, "bob 不应读到 alice 的键");
            kv_put_scoped("shared", "bob-value");
        });
        with_user_ns("alice", || {
            assert_eq!(
                kv_get_scoped("shared").as_deref(),
                Some("alice-value"),
                "alice 的值不应被 bob 覆盖"
            );
        });
        // 未设置命名空间时归入 public，同样与具名用户隔离
        assert_eq!(kv_get_scoped("shared"), None);
    }

    #[test]
    fn device_id_is_stable_per_user_and_distinct_across_users() {
        let alice_first = with_user_ns("alice", device_id);
        let alice_second = with_user_ns("alice", device_id);
        let bob = with_user_ns("bob", device_id);
        assert_eq!(alice_first, alice_second, "同一用户的 deviceID 应保持稳定");
        assert_ne!(alice_first, bob, "不同用户不应共享 deviceID");
    }

    #[test]
    fn js_cache_binding_is_isolated_between_users() {
        with_user_ns("alice", || {
            eval_js("cache.put('token', 'from-alice')", "", "").unwrap();
        });
        let miss_for_bob = with_user_ns("bob", || {
            eval_js("(cache.get('token') == null) ? 'MISS' : 'HIT'", "", "").unwrap()
        });
        assert_eq!(miss_for_bob, "MISS", "bob 不应读到 alice 写入的 cache");
        let hit_for_alice = with_user_ns("alice", || {
            eval_js("String(cache.get('token'))", "", "").unwrap()
        });
        assert_eq!(hit_for_alice, "from-alice");
    }

    // ─── 单次解析聚合 JS 预算 ───

    #[test]
    fn exhausted_parse_budget_rejects_eval_without_running_script() {
        let _budget = begin_parse_budget();
        set_parse_budget_for_test(Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(60));
        // 预算已耗尽：直接 Err，不进入脚本执行
        assert!(eval_js("1+1", "", "").is_err());
    }

    #[test]
    fn parse_budget_allows_normal_eval_and_nests() {
        let _budget = begin_parse_budget();
        assert_eq!(eval_js("1+1", "", "").unwrap(), "2");
        // 嵌套守卫：只加深深度，不重置截止时间；期间求值照常
        {
            let _nested = begin_parse_budget();
            assert_eq!(eval_js("2+2", "", "").unwrap(), "4");
        }
        assert_eq!(eval_js("3+3", "", "").unwrap(), "6");
    }

    #[test]
    fn parse_budget_cleared_after_outermost_guard_drops() {
        {
            let _budget = begin_parse_budget();
            set_parse_budget_for_test(Duration::from_millis(1));
            std::thread::sleep(Duration::from_millis(5));
            assert!(eval_js("1+1", "", "").is_err(), "预算内应当被拒");
        }
        // 最外层 guard drop 后预算清空：同一线程上求值恢复
        assert_eq!(eval_js("1+1", "", "").unwrap(), "2");
    }

    // ─── 求值统计计数器 ───
    //
    // 计数器是进程级全局状态，而 cargo test 同进程多线程并行跑用例，
    // 其他用例可能同时求值，因此一律用「前后差值 >= 1」的相对断言，
    // 不能断言精确相等。

    #[test]
    fn eval_success_increments_total_counter() {
        let before = js_eval_stats();
        assert_eq!(eval_js("1+1", "", "").unwrap(), "2");
        let after = js_eval_stats();
        assert!(
            after.total > before.total,
            "成功求值应计入总次数: before={before:?} after={after:?}"
        );
        assert!(
            after.total_micros >= before.total_micros,
            "累计耗时只增不减: before={before:?} after={after:?}"
        );
    }

    #[test]
    fn eval_failure_increments_failed_counter() {
        let before = js_eval_stats();
        assert!(eval_js("return )", "", "").is_err());
        let after = js_eval_stats();
        assert!(
            after.total > before.total,
            "失败求值同样计入总次数: before={before:?} after={after:?}"
        );
        assert!(
            after.failed > before.failed,
            "失败求值应计入失败次数: before={before:?} after={after:?}"
        );
        // 语法错误不是中断：中断计数不应因它增长（其他并行用例可能正常触发中断，
        // 故只能断言不小于）
        assert!(
            after.interrupted >= before.interrupted,
            "中断计数只增不减: before={before:?} after={after:?}"
        );
    }

    #[test]
    fn exhausted_budget_increments_budget_counter() {
        let before = js_eval_stats();
        {
            let _budget = begin_parse_budget();
            set_parse_budget_for_test(Duration::from_millis(50));
            std::thread::sleep(Duration::from_millis(60));
            assert!(eval_js("1+1", "", "").is_err(), "预算耗尽应直接拒绝");
        }
        let after = js_eval_stats();
        assert!(
            after.budget_exhausted > before.budget_exhausted,
            "预算耗尽应单独计数: before={before:?} after={after:?}"
        );
        assert!(
            after.failed > before.failed,
            "预算耗尽被拒同时计入失败次数: before={before:?} after={after:?}"
        );
    }
}
