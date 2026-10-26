//! 按键互斥锁表（singleflight）：同一键的并发任务共享一把 tokio 互斥锁，
//! 先到的任务真正执行，其余等锁后复用其结果。
//!
//! 图片管道（`image_service`）与正文回源（`book_service`）此前各自维护一份
//! 逐行相同的锁表骨架，这里收口为一份。与直接用 `BoundedMap` 的区别在
//! 逐出策略：表满时**只逐出空闲条目**（`Arc::strong_count == 1`，即只被表
//! 持有、没有任何任务持有克隆的锁）。仍在使用的锁被逐出不会伤到已持有它的
//! 任务，但后续同键请求会新建一把锁，同键并发去重（单飞）就此失效——
//! 所以宁可在「全部条目都在用」时让表暂时超过容量，也不挤掉在用锁。

use crate::util::bounded_map::BoundedMap;
use std::sync::Arc;

/// 按键互斥锁表。内部临界区是纯同步短操作，std Mutex 不跨 await。
pub struct KeyedMutex {
    locks: std::sync::Mutex<BoundedMap<Arc<tokio::sync::Mutex<()>>>>,
}

impl KeyedMutex {
    /// `max_entries` 是锁表条数上限：正常路径用完即 `release` 清理，
    /// 上限只为兜住取消/panic 遗留的条目（它们在持有者 drop 后变为
    /// 空闲，插入新条目时可被逐出）。
    pub fn new(max_entries: usize) -> Self {
        Self {
            locks: std::sync::Mutex::new(BoundedMap::new(max_entries)),
        }
    }

    /// 登记并返回 `key` 的键锁：已存在则复用（后来的等待者跟在队尾），
    /// 否则新建。插入导致超容量时只逐出空闲条目；所有条目都在用则允许
    /// 表暂时超过容量（正确性优先于容量上限），并告警一次。
    pub fn acquire(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().unwrap();
        if let Some(existing) = locks.get(key) {
            return Arc::clone(existing);
        }
        let fresh = Arc::new(tokio::sync::Mutex::new(()));
        // 只被表持有（strong_count == 1）的锁是空闲的：持有者已 drop，
        // 条目是取消/panic 的遗留或等待者走完后的残留，逐出安全
        let within_cap = locks.insert_evicting_where(key.to_string(), Arc::clone(&fresh), |lock| {
            Arc::strong_count(lock) == 1
        });
        if !within_cap {
            tracing::warn!("单飞锁表全部条目都在使用中，暂时超过容量上限");
        }
        fresh
    }

    /// 用完清理：表里还是这把锁（`Arc::ptr_eq`）、且没有别的等待者持有
    /// 克隆（`strong_count <= 2`：表 + 调用者各一份）时摘掉条目。
    pub fn release(&self, key: &str, lock: &Arc<tokio::sync::Mutex<()>>) {
        let mut locks = self.locks.lock().unwrap();
        let unused = match locks.get(key) {
            Some(current) => Arc::ptr_eq(current, lock) && Arc::strong_count(current) <= 2,
            None => false,
        };
        if unused {
            locks.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn same_key_returns_same_lock() {
        let table = KeyedMutex::new(4);
        let a = table.acquire("k1");
        let b = table.acquire("k1");
        assert!(Arc::ptr_eq(&a, &b), "同键必须返回同一把锁");
    }

    #[tokio::test]
    async fn different_keys_get_different_locks() {
        let table = KeyedMutex::new(4);
        let a = table.acquire("k1");
        let b = table.acquire("k2");
        assert!(!Arc::ptr_eq(&a, &b), "不同键必须是不同的锁");
    }

    #[tokio::test]
    async fn eviction_skips_in_use_locks() {
        let table = KeyedMutex::new(2);
        let idle = table.acquire("idle");
        let idle_weak = Arc::downgrade(&idle);
        let in_use = table.acquire("in-use");
        // idle 的持有者 drop 后，表里的条目只被表持有（strong_count == 1）→ 空闲
        drop(idle);
        assert!(idle_weak.upgrade().is_some(), "条目还在表里，锁本体仍存活");
        // 表已满，插入第三键：空闲的 idle 条目被逐出，在用的 in-use 条目必须保留
        let third = table.acquire("k3");
        assert!(
            idle_weak.upgrade().is_none(),
            "空闲条目应被逐出，表持有的最后一份 Arc 随之销毁"
        );
        let still_same = table.acquire("in-use");
        assert!(
            Arc::ptr_eq(&in_use, &still_same),
            "在用锁不得被逐出，同键再取必须还是同一把"
        );
        drop(third);
        drop(still_same);
    }

    #[tokio::test]
    async fn all_in_use_allows_overflow_without_evicting() {
        let table = KeyedMutex::new(2);
        let a = table.acquire("a");
        let b = table.acquire("b");
        // 两个条目都在用：第三键插入不逐出任何条目，允许超容量
        let c = table.acquire("c");
        let a2 = table.acquire("a");
        let b2 = table.acquire("b");
        assert!(Arc::ptr_eq(&a, &a2), "全部在用时不许逐出 a");
        assert!(Arc::ptr_eq(&b, &b2), "全部在用时不许逐出 b");
        assert!(!Arc::ptr_eq(&a, &c));
    }

    #[tokio::test]
    async fn release_removes_idle_entry() {
        let table = KeyedMutex::new(4);
        let lock = table.acquire("k");
        table.release("k", &lock);
        let again = table.acquire("k");
        assert!(
            !Arc::ptr_eq(&lock, &again),
            "release 后条目被摘除，同键再取是新锁"
        );
    }

    #[tokio::test]
    async fn release_keeps_entry_while_waiters_hold_clones() {
        let table = KeyedMutex::new(4);
        let first = table.acquire("k");
        let waiter = table.acquire("k");
        // 表 + first + waiter 共三份：strong_count > 2，release 不该摘条目
        table.release("k", &first);
        drop(first);
        let again = table.acquire("k");
        assert!(
            Arc::ptr_eq(&waiter, &again),
            "有等待者持有克隆时 release 不摘条目，同键仍复用同一把锁"
        );
        drop(waiter);
        // 等待者走完后只剩表 + again 两份：release 摘掉条目
        table.release("k", &again);
        let fresh = table.acquire("k");
        assert!(!Arc::ptr_eq(&again, &fresh));
    }
}
