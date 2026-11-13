use chrono::Utc;

pub fn now_ts() -> i64 {
    Utc::now().timestamp()
}

pub fn now_millis() -> i64 {
    Utc::now().timestamp_millis()
}

/// Unix 秒 → RFC 3339 UTC（秒级精度，契约 §5.2「秒级足够」）。
/// `/v1` 的时间字段统一用这个口径；0 与越界值都渲染为 epoch。
pub fn rfc3339_secs(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).expect("epoch is valid"))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}
