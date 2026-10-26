//! 有界键值缓存：满时按 FIFO 逐出最早插入的条目。
//!
//! 此前各进程内缓存（正则编译、JS cache/kv、jsLib、exploreUrl 求值）
//! 都是「满即整表清空」——高峰期一次溢出会让全部条目同时失效，
//! 触发整批重新编译/重新求值。FIFO 逐出只牺牲最旧的一小批。

use std::collections::{HashMap, VecDeque};

pub struct BoundedMap<V> {
    map: HashMap<String, V>,
    order: VecDeque<String>,
    max: usize,
    /// 可选的字节上限：条目值按 `size_of` 记账，超出时同样 FIFO 逐出。
    /// JS KV 的 value 由书源决定大小，只限条数时内存仍可无界增长。
    max_bytes: Option<usize>,
    total_bytes: usize,
}

impl<V> BoundedMap<V> {
    pub fn new(max: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            max,
            max_bytes: None,
            total_bytes: 0,
        }
    }
}

/// 带字节上限的变体：值用 `AsRef<[u8]>` 计量。
impl<V: AsRef<[u8]>> BoundedMap<V> {
    pub fn with_bytes(max: usize, max_bytes: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            max,
            max_bytes: Some(max_bytes),
            total_bytes: 0,
        }
    }

    pub fn insert_sized(&mut self, key: String, value: V) {
        let value_len = value.as_ref().len();
        // 旧值的字节账先摘掉（覆盖插入场景）
        if let Some(old) = self.map.get(&key) {
            self.total_bytes = self.total_bytes.saturating_sub(old.as_ref().len());
        }
        self.insert(key, value);
        self.total_bytes += value_len;
        if let Some(max_bytes) = self.max_bytes {
            while self.total_bytes > max_bytes {
                let Some(old_key) = self.order.pop_front() else {
                    break;
                };
                if let Some(old) = self.map.remove(&old_key) {
                    self.total_bytes = self.total_bytes.saturating_sub(old.as_ref().len());
                }
            }
        }
    }

    #[cfg(test)]
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}

impl<V> BoundedMap<V> {
    pub fn get(&self, key: &str) -> Option<&V> {
        self.map.get(key)
    }

    /// 当前条数（单飞锁表的溢出告警与节流判定用）。
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// 是否为空表。
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 删除指定键（TTL 过期剔除用）。
    pub fn remove(&mut self, key: &str) -> Option<V> {
        self.order.retain(|k| k != key);
        self.map.remove(key)
    }

    #[cfg(test)]
    pub fn contains_key(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    /// 插入条目；键已存在时不更新插入顺序（与原「命中即续期」的无序
    /// 行为一致，仅容量控制走 FIFO）。
    pub fn insert(&mut self, key: String, value: V) {
        if !self.map.contains_key(&key) {
            while self.map.len() >= self.max {
                let evicted = self.order.pop_front();
                match evicted {
                    Some(old) => {
                        self.map.remove(&old);
                    }
                    None => break,
                }
            }
            self.order.push_back(key.clone());
        }
        self.map.insert(key, value);
    }

    /// 插入条目；容量满时从最旧一端起逐出满足 `evictable` 的条目，
    /// 不满足条件的条目一律保留。没有可逐出条目时允许暂时超过容量
    /// （由调用方权衡：正确性优先于容量上限），并返回 `false`；
    /// 插入后条数在上限内则返回 `true`。
    ///
    /// 与 `insert` 一样不做字节记账，字节上限场景请走 `insert_sized`。
    /// 单飞锁表（`keyed_mutex`）用它避免把仍在使用的锁条目挤出表。
    pub fn insert_evicting_where(
        &mut self,
        key: String,
        value: V,
        evictable: impl Fn(&V) -> bool,
    ) -> bool {
        let mut within_cap = true;
        if !self.map.contains_key(&key) {
            while self.map.len() >= self.max {
                let Some(pos) = self
                    .order
                    .iter()
                    .position(|k| self.map.get(k).is_some_and(&evictable))
                else {
                    within_cap = false;
                    break;
                };
                let old = self.order.remove(pos).expect("position 来自 order 自身");
                self.map.remove(&old);
            }
            self.order.push_back(key.clone());
        }
        self.map.insert(key, value);
        within_cap
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_evicts_oldest_entry_only() {
        let mut m = BoundedMap::new(3);
        m.insert("a".into(), 1);
        m.insert("b".into(), 2);
        m.insert("c".into(), 3);
        m.insert("d".into(), 4);
        assert!(!m.contains_key("a"), "最早的 a 应被逐出");
        assert_eq!(m.get("b"), Some(&2));
        assert_eq!(m.get("d"), Some(&4));
        // 重复插入不挤占顺序，也不应扩大容量
        m.insert("d".into(), 40);
        m.insert("e".into(), 5);
        assert_eq!(m.get("d"), Some(&40));
        assert!(!m.contains_key("b"), "b 应按插入顺序被逐出");
        assert!(m.contains_key("c"));
    }

    #[test]
    fn byte_cap_evicts_oldest_until_under_budget() {
        let mut m: BoundedMap<String> = BoundedMap::with_bytes(100, 10);
        m.insert_sized("a".into(), "12345".into());
        m.insert_sized("b".into(), "12345".into());
        assert_eq!(m.total_bytes(), 10);
        m.insert_sized("c".into(), "12345".into());
        assert!(!m.contains_key("a"), "超字节上限应逐出最旧的 a");
        assert_eq!(m.total_bytes(), 10);
        // 覆盖插入先摘旧账
        m.insert_sized("b".into(), "12".into());
        assert_eq!(m.total_bytes(), 7);
    }

    #[test]
    fn empty_map_never_stalls() {
        let mut m = BoundedMap::new(1);
        m.insert("a".into(), 1);
        m.insert("b".into(), 2);
        assert!(m.contains_key("b"));
        assert_eq!(m.order.len(), 1);
    }

    #[test]
    fn insert_evicting_where_skips_protected_entries() {
        // 奇数值受保护（模拟「在用」），偶数值可逐出（模拟「空闲」）
        let mut m = BoundedMap::new(3);
        let evict_even = |v: &i32| v % 2 == 0;
        assert!(m.insert_evicting_where("a".into(), 2, evict_even));
        assert!(m.insert_evicting_where("b".into(), 1, evict_even));
        assert!(m.insert_evicting_where("c".into(), 4, evict_even));
        // 满员后插入：a（最旧且可逐出）被逐出，b 虽更旧但受保护时也不该轮到——
        // 这里 a 比 b 旧，先逐 a；b/c 保留
        assert!(m.insert_evicting_where("d".into(), 6, evict_even));
        assert!(!m.contains_key("a"));
        assert!(m.contains_key("b"));
        assert!(m.contains_key("c"));
        assert!(m.contains_key("d"));
        // 现在 a 已走，最旧的是受保护的 b，逐出应跳过它去摘 c
        assert!(m.insert_evicting_where("e".into(), 8, evict_even));
        assert!(m.contains_key("b"), "受保护条目必须保留");
        assert!(!m.contains_key("c"));
        assert!(m.contains_key("d"));
        assert!(m.contains_key("e"));
    }

    #[test]
    fn insert_evicting_where_allows_overflow_when_nothing_evictable() {
        let mut m = BoundedMap::new(2);
        let none_evictable = |_: &i32| false;
        assert!(m.insert_evicting_where("a".into(), 1, none_evictable));
        assert!(m.insert_evicting_where("b".into(), 2, none_evictable));
        // 全部不可逐出：允许超容量，返回 false 提示调用方
        assert!(!m.insert_evicting_where("c".into(), 3, none_evictable));
        assert!(m.contains_key("a"));
        assert!(m.contains_key("b"));
        assert!(m.contains_key("c"));
        assert_eq!(m.map.len(), 3);
        // 覆盖已存在的键不受容量逻辑影响
        assert!(m.insert_evicting_where("c".into(), 30, none_evictable));
        assert_eq!(m.get("c"), Some(&30));
        assert_eq!(m.map.len(), 3);
    }

    #[test]
    fn insert_evicting_where_recovers_once_entry_becomes_evictable() {
        // 模拟「锁释放后重新变得可逐出」：先全部在用撑爆容量，
        // 之后有条目变为空闲，插入又能逐出它回收空间
        let mut m: BoundedMap<i32> = BoundedMap::new(2);
        let none = |_: &i32| false;
        m.insert_evicting_where("a".into(), 1, none);
        m.insert_evicting_where("b".into(), 2, none);
        assert!(!m.insert_evicting_where("c".into(), 3, none));
        assert_eq!(m.map.len(), 3);
        // b 变为空闲（偶数可逐出）：插入 d 逐出最旧可逐出的 b，
        // 但 a/c 仍不可逐出，条数停在 3 并返回 false
        let even = |v: &i32| v % 2 == 0;
        assert!(!m.insert_evicting_where("d".into(), 4, even));
        assert!(!m.contains_key("b"));
        assert!(m.contains_key("a"));
        assert!(m.contains_key("c"));
        assert!(m.contains_key("d"));
        assert_eq!(m.map.len(), 3);
        // 其余条目也变空闲后，插入 e 逐出 a、c，回到上限内
        let any = |_: &i32| true;
        assert!(m.insert_evicting_where("e".into(), 5, any));
        assert_eq!(m.map.len(), 2);
        assert!(m.contains_key("d"));
        assert!(m.contains_key("e"));
    }
}
