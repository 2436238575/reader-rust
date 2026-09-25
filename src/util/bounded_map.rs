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
}

impl<V> BoundedMap<V> {
    pub fn new(max: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            max,
        }
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.map.get(key)
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
    fn empty_map_never_stalls() {
        let mut m = BoundedMap::new(1);
        m.insert("a".into(), 1);
        m.insert("b".into(), 2);
        assert!(m.contains_key("b"));
        assert_eq!(m.order.len(), 1);
    }
}
