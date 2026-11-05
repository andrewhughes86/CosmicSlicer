use cpp_map::linkedlist::LinkedList;
use cpp_map::prelude::*;
use rand::{Rng, SeedableRng, rngs::StdRng};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Debug;

// Helper function to create a list with random data
#[cfg(test)]
#[allow(dead_code)]
pub fn skp_create_test_data(
    insert_count: usize,
    remove_count: usize,
    seed_add: u64,
) -> (SkipList<i32, String>, BTreeMap<i32, String>) {
    let mut rng = StdRng::seed_from_u64(4 + seed_add); // Fixed seed for deterministic tests
    let mut skiplist = SkipList::<i32, String>::default();
    let mut btreemap = BTreeMap::new();
    let mut inserted_keys = Vec::new();
    let mut remove_count = remove_count;

    // Insert phase
    for _ in 0..insert_count {
        let key = rng.random_range(0..1000);
        let value = format!("value_{key}");

        // Insert into both structures
        let _ = skiplist.insert(key, value.clone());
        btreemap.entry(key).or_insert(value);
        if rng.random_bool(0.01) {
            // randomly remove a just inserted item
            remove_count -= 1;
            skiplist.remove(&key);
            btreemap.remove(&key);
        }
        inserted_keys.push(key);
    }

    // Removal phase - randomly remove some items
    for _ in 0..remove_count.min(inserted_keys.len()) {
        let idx = rng.random_range(0..inserted_keys.len());
        let key = inserted_keys.remove(idx);

        skiplist.remove(&key);
        btreemap.remove(&key);
    }

    let mut iter1 = skiplist.iter();
    let mut iter2 = btreemap.iter();
    loop {
        match (iter1.next(), iter2.next()) {
            (Some(x), Some(y)) => assert_eq!(x.0, y.0),
            (None, None) => break,
            _ => panic!(),
        }
    }

    //test_free_pool_uniqueness(&list);
    (skiplist, btreemap)
}

// Helper function to create a list with random data
#[cfg(test)]
#[allow(dead_code)]
pub fn llt_create_test_data(
    insert_count: usize,
    remove_count: usize,
    seed_add: u64,
) -> (LinkedList<i32, String>, BTreeMap<i32, String>) {
    let mut rng = StdRng::seed_from_u64(4 + seed_add); // Fixed seed for deterministic tests
    let mut linkedlist = LinkedList::<i32, String>::default();
    let mut btreemap = BTreeMap::new();
    let mut inserted_keys = Vec::new();
    let mut remove_count = remove_count;

    // Insert phase
    for _ in 0..insert_count {
        let key = rng.random_range(0..1000);
        let value = format!("value_{key}");

        // Insert into both structures
        let _ = linkedlist.insert(key, value.clone());
        btreemap.entry(key).or_insert(value);
        if rng.random_bool(0.01) {
            // randomly remove a just inserted item
            remove_count -= 1;
            linkedlist.remove(&key);
            btreemap.remove(&key);
        }
        inserted_keys.push(key);
    }

    // Removal phase - randomly remove some items
    for _ in 0..remove_count.min(inserted_keys.len()) {
        let idx = rng.random_range(0..inserted_keys.len());
        let key = inserted_keys.remove(idx);

        linkedlist.remove(&key);
        btreemap.remove(&key);
    }

    let mut iter1 = linkedlist.iter();
    let mut iter2 = btreemap.iter();
    loop {
        match (iter1.next(), iter2.next()) {
            (Some(x), Some(y)) => assert_eq!(x.0, y.0),
            (None, None) => break,
            _ => panic!(),
        }
    }

    //test_free_pool_uniqueness(&list);
    (linkedlist, btreemap)
}

#[cfg(test)]
#[allow(dead_code)]
pub fn skp_test_head_and_tail_is_some<K, V>(list: &SkipList<K, V>)
where
    K: Debug + Ord + Clone + IsLessThan,
    V: Debug + Clone,
{
    assert!(list.first().is_some());
    assert!(list.last().is_some());
}
#[cfg(test)]
#[allow(dead_code)]
pub fn skp_test_head_and_tail_is_none<K, V>(list: &SkipList<K, V>)
where
    K: Debug + Ord + Clone + IsLessThan,
    V: Debug + Clone,
{
    assert!(list.first().is_none());
    assert!(list.last().is_none());
}

#[cfg(test)]
#[allow(dead_code)]
pub fn llt_test_head_and_tail_is_some<K, V>(list: &LinkedList<K, V>)
where
    K: Debug + Ord + Clone + IsLessThan,
    V: Debug + Clone,
{
    assert!(list.first().is_some());
    assert!(list.last().is_some());
}

#[cfg(test)]
#[allow(dead_code)]
pub fn llt_test_head_and_tail_is_none<K, V>(list: &LinkedList<K, V>)
where
    K: Debug + Ord + Clone + IsLessThan,
    V: Debug + Clone,
{
    assert!(list.first().is_none());
    assert!(list.last().is_none());
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Eq)]
pub struct RoundedTen(pub i32);

impl Ord for RoundedTen {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.0 / 10).cmp(&(other.0 / 10))
    }
}

impl PartialOrd for RoundedTen {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for RoundedTen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// Eq is *not* the same as cmp() == Ordering.Equal in this case
impl PartialEq for RoundedTen {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl IsLessThan for RoundedTen {
    fn is_less_than(&self, other: &Self) -> bool {
        self < other
    }
}
