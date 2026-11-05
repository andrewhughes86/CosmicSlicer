use cpp_map::CppMapError;
use cpp_map::linkedlist::LinkedList;
use cpp_map::prelude::*;
use rand::{Rng, SeedableRng, rngs::StdRng};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Debug;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Eq)]
struct RoundedTen(i32);

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

// Helper function to create a list with random data
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

    assert_eq!(skiplist.len(), btreemap.len());

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

fn main() -> Result<(), CppMapError> {
    // loop to account for the random properties of a skip list
    #[allow(clippy::unnecessary_literal_unwrap)]
    #[cfg(any(test, debug_assertions))]
    for _i in 0..1 {
        let mut list = LinkedList::default();
        let _ = list.insert(1, "one")?;
        list.validate();
        let _ = list.insert(2, "two")?;
        list.validate();
        let _ = list.insert(3, "three")?;
        list.validate();
        list.debug_print();
        assert_eq!(list.remove(&2), Some((2, "two")));
        list.validate();
        assert_eq!(list.get(&2), None);

        let _ = list.insert(4, "four")?;
        list.validate();
        assert_eq!(list.get(&4), Some(&"four"));

        assert_eq!(list.remove(&1), Some((1, "one")));
        list.validate();
        //test_free_pool_uniqueness(&list);
        assert_eq!(list.remove(&3), Some((3, "three")));
        list.validate();
        //test_free_pool_uniqueness(&list);
        //test_head_and_tail_is_some(&list);
        assert_eq!(list.remove(&4), Some((4, "four")));
        list.validate();
        //test_free_pool_uniqueness(&list);

        // List should be empty now
        //test_head_and_tail_is_none(&list);
        list.clear();
        list.validate();
        println!("cleared");
        println!();
        let _ = list.insert(3, "three")?;
        print!("result:");
        list.debug_print();
        list.validate();
        println!();
        let _ = list.insert(9, "nine")?;
        list.validate();
        print!("result:");
        list.debug_print();
        println!();
        let _ = list.insert(5, "five")?;
        list.validate();
        print!("result:");
        list.debug_print();
        println!();
        let _ = list.insert(1, "one")?;
        print!("result:");
        list.debug_print();
        list.validate();
        println!();
        let _ = list.insert(8, "eight")?;
        list.validate();
        print!("result:");
        list.debug_print();
        println!();
        let _ = list.insert(0, "zero")?;
        print!("result:");
        list.debug_print();
        list.validate();
        println!();
        let _ = list.insert(2, "two")?;
        list.validate();
        print!("result:");
        list.debug_print();
        println!();
        let _ = list.insert(4, "four")?;
        list.validate();
        print!("result:");
        list.debug_print();
        println!();
        let _ = list.insert(6, "six")?;
        list.validate();
        print!("result:");
        list.debug_print();
        println!();
        let _ = list.insert(7, "seven")?;
        list.validate();
        print!("result:");
        list.debug_print();

        println!();
        println!("removing");
        let _ = list.remove(&7);
        list.debug_print();
        list.validate();
        let _ = list.remove(&1);
        list.debug_print();
        list.validate();
        let _ = list.remove(&3);
        list.debug_print();
        list.validate();
        let _ = list.remove(&8);
        list.debug_print();
        list.validate();
        let _ = list.remove(&2);
        list.debug_print();
        list.validate();
        let _ = list.remove(&4);
        list.debug_print();
        list.validate();
        let _ = list.remove(&5);
        list.debug_print();
        list.validate();
        let _ = list.remove(&6);
        list.debug_print();
        list.validate();
    }
    Ok(())
}
