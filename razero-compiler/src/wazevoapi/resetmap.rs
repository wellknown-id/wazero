//! Helpers to reuse existing map allocations.

use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasher, Hash};

pub trait ClearableMap {
    fn clear_map(&mut self);
}

impl<K, V, S> ClearableMap for HashMap<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher,
{
    fn clear_map(&mut self) {
        self.clear();
    }
}

impl<K, V> ClearableMap for BTreeMap<K, V>
where
    K: Ord,
{
    fn clear_map(&mut self) {
        self.clear();
    }
}

pub fn reset_map<M>(map: &mut Option<M>) -> &mut M
where
    M: ClearableMap + Default,
{
    let map = map.get_or_insert_with(M::default);
    map.clear_map();
    map
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::reset_map;

    #[test]
    fn reset_map_creates_then_clears_hash_map() {
        let mut map: Option<HashMap<u32, u32>> = None;
        reset_map(&mut map).insert(1, 2);
        assert_eq!(map.as_ref().unwrap().get(&1), Some(&2));

        let returned = reset_map(&mut map);
        assert!(returned.is_empty());
    }
}
