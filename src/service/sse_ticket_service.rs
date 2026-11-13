//! SSE 票据（契约 §6.4/D11）：给拿不到 HttpOnly Cookie 的客户端一张一次性通行证。
//!
//! 浏览器同源 `EventSource` 本来就会带 Cookie，票据是给 curl / 移动端 / 跨设备
//! 导出用的。票 60 秒有效、**一次性**、绑定单个资源路径——查询串会进反代访问
//! 日志与浏览器历史，所以把暴露面压到「一个路径、一次、一分钟」。
//!
//! 只由 `POST /v1/sse-tickets` 签发；校验在 `/v1` 的鉴权中间件里（Cookie/Bearer
//! 都没有时才看 `?ticket=`，且只认事件流路径）。

use crate::auth::extractor::AuthUser;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const SSE_TICKET_TTL: Duration = Duration::from_secs(60);
/// 每用户未消费票据上限（60 秒窗口内足够重连/多标签页）。
const MAX_TICKETS_PER_USER: usize = 8;
/// 全局未消费票据上限（防票据表无限增长）。
const MAX_TICKETS_TOTAL: usize = 1024;

struct Ticket {
    owner: AuthUser,
    path: String,
    expires_at: Instant,
}

pub struct SseTicketService {
    inner: Mutex<HashMap<String, Ticket>>,
    ttl: Duration,
}

impl Default for SseTicketService {
    fn default() -> Self {
        Self::new()
    }
}

impl SseTicketService {
    pub fn new() -> Self {
        Self::with_ttl(SSE_TICKET_TTL)
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    /// 签发一张绑定 `path` 的票，返回 `(ticket, expiresAt 的 Unix 秒)`。
    /// 每用户/全局未消费票据超限 → 429 `RATE_LIMITED`（`Retry-After` 60）。
    pub fn issue(
        &self,
        owner: &AuthUser,
        path: &str,
    ) -> Result<(String, i64), crate::api::problem::Problem> {
        use crate::api::problem::{ErrorCode, Problem};
        let now = Instant::now();
        let expires_at = now + self.ttl;
        let ticket = format!("sst_{}", uuid::Uuid::new_v4().simple());
        let mut inner = self.lock();
        inner.retain(|_, entry| entry.expires_at > now);
        let user_count = inner
            .values()
            .filter(|entry| entry.owner.ns == owner.ns)
            .count();
        if user_count >= MAX_TICKETS_PER_USER || inner.len() >= MAX_TICKETS_TOTAL {
            return Err(Problem::new(ErrorCode::RateLimited)
                .detail("未消费的 SSE 票据过多，请稍后再试")
                .retry_after(60));
        }
        inner.insert(
            ticket.clone(),
            Ticket {
                owner: owner.clone(),
                path: path.to_string(),
                expires_at,
            },
        );
        // Instant 无法直接转 Unix 秒：用 TTL 反推（签发时刻的墙钟 + ttl）
        let expires_epoch = crate::util::time::now_ts() + self.ttl.as_secs() as i64;
        Ok((ticket, expires_epoch))
    }

    /// 消费一张票：路径必须完全一致，且只能用一次。失败返回 None。
    pub fn consume(&self, ticket: &str, path: &str) -> Option<AuthUser> {
        let now = Instant::now();
        let mut inner = self.lock();
        inner.retain(|_, entry| entry.expires_at > now);
        let entry = inner.get(ticket)?;
        if entry.expires_at <= now || entry.path != path {
            return None;
        }
        // 一次性：请求路径匹配才消费；不匹配时不消耗（避免拿错路径试探烧票）
        inner.remove(ticket).map(|entry| entry.owner)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Ticket>> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user() -> AuthUser {
        AuthUser {
            uid: 1,
            username: "reader".into(),
            ns: "user_1".into(),
            ver: 1,
            scope: None,
            proxy_source: None,
        }
    }

    #[test]
    fn ticket_is_single_use_and_path_bound() {
        let service = SseTicketService::new();
        let (ticket, expires_at) = service.issue(&user(), "/v1/tasks/tsk_1/events").unwrap();
        assert!(ticket.starts_with("sst_"));
        assert!(expires_at > 0);

        // 路径不匹配不消耗
        assert!(service.consume(&ticket, "/v1/tasks/tsk_2/events").is_none());
        let consumed = service
            .consume(&ticket, "/v1/tasks/tsk_1/events")
            .expect("票据应有效");
        assert_eq!(consumed.ns, "user_1");
        // 二次使用失败
        assert!(service.consume(&ticket, "/v1/tasks/tsk_1/events").is_none());
    }

    #[test]
    fn per_user_ticket_quota_is_enforced() {
        let service = SseTicketService::new();
        for _ in 0..MAX_TICKETS_PER_USER {
            service.issue(&user(), "/v1/tasks/tsk_1/events").unwrap();
        }
        let error = service
            .issue(&user(), "/v1/tasks/tsk_1/events")
            .expect_err("超过每用户票据上限应被拒绝");
        assert_eq!(error.code(), crate::api::problem::ErrorCode::RateLimited);
    }

    #[test]
    fn expired_ticket_is_rejected() {
        let service = SseTicketService::with_ttl(Duration::from_millis(1));
        let (ticket, _) = service.issue(&user(), "/v1/tasks/tsk_1/events").unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert!(service.consume(&ticket, "/v1/tasks/tsk_1/events").is_none());
    }
}
