use chrono::Utc;

pub fn now_ts() -> i64 {
    Utc::now().timestamp()
}

pub fn now_millis() -> i64 {
    Utc::now().timestamp_millis()
}
