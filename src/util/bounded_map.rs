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
}
