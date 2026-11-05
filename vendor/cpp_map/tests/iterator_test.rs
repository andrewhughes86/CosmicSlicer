// SPDX-License-Identifier: MIT OR Apache-2.0

// Copyright 2025 Eadf (github.com/eadf)
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use cpp_map::prelude::*;
use std::collections::BTreeMap;

mod common;
use common::*;

#[test]
fn test_forward_iterator() {
    let (list, map) = llt_create_test_data(500, 20, 1);

    // Compare forward iteration
    let list_items: Vec<_> = list.iter().collect();
    let map_items: Vec<_> = map.iter().collect();

    assert_eq!(list_items.len(), map_items.len());

    for ((l_k, l_v), (m_k, m_v)) in list_items.iter().zip(map_items.iter()) {
        assert_eq!(*l_k, *m_k);
        assert_eq!(*l_v, *m_v);
    }
}

#[test]
fn test_forward_iterator2() {
    let (list, map) = skp_create_test_data(500, 20, 11);

    // Compare forward iteration using IntoIterator(&BRreeLIst)
    let mut list_items: Vec<_> = Vec::with_capacity(list.len());
    for i in list.iter() {
        list_items.push(i);
    }
    let map_items: Vec<_> = map.iter().collect();

    assert_eq!(list_items.len(), map_items.len());

    for ((l_k, l_v), (m_k, m_v)) in list_items.iter().zip(map_items.iter()) {
        assert_eq!(*l_k, *m_k);
        assert_eq!(*l_v, *m_v);
    }
}

/*
#[test]
fn test_forward_iterator3() {
    let (list, map) = create_test_data(500, 20, 22);

    // Compare forward iteration using IntoIterator(BRreeLIst)
    let list_items: Vec<_> = list.into_iter().collect();
    let map_items: Vec<_> = map.iter().collect();

    assert_eq!(list_items.len(), map_items.len());

    for ((l_k, l_v), (m_k, m_v)) in list_items.iter().zip(map_items.iter()) {
        assert_eq!(*l_k, *m_k);
        assert_eq!(*l_v, *m_v);
    }
}

#[test]
fn test_reverse_iterator() {
    let (list, map) = create_test_data(500, 30, 2);

    // Compare reverse iteration
    let list_items: Vec<_> = list.iter().rev().collect();
    let mut map_items: Vec<_> = map.iter().rev().collect();

    assert_eq!(list_items.len(), map_items.len());

    for ((l_k, l_v), (m_k, m_v)) in list_items.iter().zip(map_items.iter_mut()) {
        assert_eq!(*l_k, *m_k);
        assert_eq!(*l_v, *m_v);
    }
}


#[test]
fn test_iter_from() {
    let (list, map) = create_test_data(500, 40, 3);
    let (start_key, _) = map.iter().take(200).next().unwrap();

    // Get iterator starting from key
    if let Some(list_iter) = list.iter_from(start_key) {
        let list_items: Vec<_> = list_iter.collect();

        // Get equivalent items from BTreeMap
        let map_items: Vec<_> = map.range(start_key..).collect();

        assert_eq!(list_items.len(), map_items.len());

        for ((l_k, l_v), (m_k, m_v)) in list_items.iter().zip(map_items.iter()) {
            assert_eq!(*l_k, *m_k);
            assert_eq!(*l_v, *m_v);
        }
    } else {
        panic!("iter_from returned None for existing key range");
    }
}

#[test]
fn test_iter_rev_from() {
    let (list, map) = create_test_data(500, 50, 4);
    let (start_key, _) = map.iter().take(200).next().unwrap();

    // Get reverse iterator starting from key
    if let Some(list_iter) = list.iter_from(start_key) {
        let list_items: Vec<_> = list_iter.rev().collect();

        // Get equivalent items from BTreeMap (need to handle ranges differently for reverse)
        let map_items: Vec<_> = map.range(start_key..).rev().collect();

        assert_eq!(list_items.len(), map_items.len());

        for ((l_k, l_v), (m_k, m_v)) in list_items.iter().zip(map_items.iter()) {
            assert_eq!(*l_k, *m_k);
            assert_eq!(*l_v, *m_v);
        }
    } else {
        panic!("iter_rev_from returned None for existing key range");
    }
}

#[test]
fn test_iter_from_key() {
    let (list, btree) = create_test_data(100, 60, 5);
    let mut bi = btree.iter();
    let (b_key, b_value) = bi.by_ref().take(100).next().unwrap();

    // Test with key that doesn't exist but is within range
    assert!(list.iter_from(&-1).is_none()); // Before first key
    assert!(list.iter_from(&2000).is_none()); // After last key
    assert!(list.iter_from(b_key).is_some()); // at a_key
    let mut li = list.iter_from(b_key).unwrap();

    let (l_key, l_v) = li.next().unwrap();
    assert_eq!(*b_key, *l_key);
    assert_eq!(*b_value, *l_v);

    for (b_key, b_value) in bi {
        let (l_key, l_v) = li.next().unwrap();
        assert_eq!(*b_key, *l_key);
        assert_eq!(*b_value, *l_v);
    }
}
*/

#[test]
fn test_duplicate_inserts_ignored() {
    let mut list = SkipList::default();
    let mut map = BTreeMap::new();

    // Insert same key multiple times
    for i in 0..10 {
        let _ = list.insert(42, format!("value_{i}"));
        map.entry(42).or_insert_with(|| format!("value_{i}"));
    }

    // Should only have one entry with the first value
    assert_eq!(list.iter().count(), 1);
    assert_eq!(list.get(&42), Some(&"value_0".to_string()));

    // Compare with BTreeMap behavior
    let list_items: Vec<_> = list.iter().collect();
    let map_items: Vec<_> = map.iter().collect();
    assert_eq!(list_items, map_items);
}
