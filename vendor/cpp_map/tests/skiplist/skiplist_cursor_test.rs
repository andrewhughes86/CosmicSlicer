// SPDX-License-Identifier: MIT OR Apache-2.0

// Copyright 2025 Eadf (github.com/eadf)
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use cpp_map::prelude::*;
mod common;
use common::*;
use cpp_map::CppMapError;

#[test]
fn test_forward_cursor() {
    let (list, map) = skp_create_test_data(500, 20, 7);

    // Compare forward iteration
    let mut cursor = list.first();
    let mut list_items = Vec::new();
    list_items.push(cursor.unwrap());
    while let Some(next_pos) = list.next_pos(cursor) {
        list_items.push(next_pos);
        cursor = Some(next_pos);
    }
    let map_items: Vec<_> = map.iter().collect();
    assert_eq!(list_items.len(), map_items.len());

    for (idx, (m_k, m_v)) in list_items.iter().zip(map_items.iter()) {
        let kv = list.get_at(*idx);
        assert_eq!(kv, Some((*m_k, *m_v)));
    }
}

#[test]
fn test_reverse_cursor() {
    let (list, map) = skp_create_test_data(500, 30, 8);

    // Compare reverse iteration
    let mut cursor = list.last();
    let mut list_items = Vec::new();
    list_items.push(cursor.unwrap());
    while let Some(prev_pos) = list.prev_pos(cursor) {
        list_items.push(prev_pos);
        cursor = Some(prev_pos);
    }
    let map_items: Vec<_> = map.iter().rev().collect();

    assert_eq!(list_items.len(), map_items.len());

    for (idx, (m_k, m_v)) in list_items.iter().zip(map_items.iter()) {
        let kv = list.get_at(*idx);
        assert_eq!(kv, Some((*m_k, *m_v)));
    }
}

#[test]
fn test_cursor_from() {
    let (skiplist, btreemap) = skp_create_test_data(500, 40, 9);
    let (start_key, _) = btreemap.iter().take(200).next().unwrap();

    // Get cursor starting from key
    let mut cursor = skiplist.lower_bound(start_key);
    assert!(skiplist.is_pos_valid(cursor));
    let mut list_items = Vec::new();
    list_items.push(skiplist.get_at(cursor.unwrap()).unwrap());
    while let Some(next_pos) = skiplist.next_pos(cursor) {
        list_items.push(skiplist.get_at(next_pos).unwrap());
        cursor = Some(next_pos);
    }

    // Get equivalent items from BTreeMap
    let map_items: Vec<_> = btreemap.range(start_key..).collect();

    assert_eq!(list_items.len(), map_items.len());

    for ((l_k, l_v), (m_k, m_v)) in list_items.iter().zip(map_items.iter()) {
        assert_eq!(l_k, m_k);
        assert_eq!(l_v, m_v);
    }
}

#[test]
fn test_cursor_rev_from() {
    let (skiplist, btreemap) = skp_create_test_data(500, 50, 10);
    let (start_key, _) = btreemap.iter().take(300).next().unwrap();

    // Get reverse cursor starting from key
    let mut cursor = skiplist.lower_bound(start_key);
    assert!(skiplist.is_pos_valid(cursor));
    let mut list_items = Vec::new();
    list_items.push(skiplist.get_at(cursor.unwrap()).unwrap());
    while let Some(prev_pos) = skiplist.prev_pos(cursor) {
        list_items.push(skiplist.get_at(prev_pos).unwrap());
        cursor = Some(prev_pos);
    }

    // Get equivalent items from BTreeMap (need to handle ranges differently for reverse)
    let map_items: Vec<_> = btreemap.range(..=start_key).rev().collect();

    assert_eq!(list_items.len(), map_items.len());

    for ((l_k, l_v), (m_k, m_v)) in list_items.iter().zip(map_items.iter()) {
        assert_eq!(l_k, m_k);
        assert_eq!(l_v, m_v);
    }
}

#[test]
fn test_empty_cursors() {
    let list: SkipList<i32, String> = SkipList::default();

    assert!(!list.is_pos_valid(list.first()));
    assert!(!list.is_pos_valid(list.last()));
    assert!(list.lower_bound(&0).is_none());
}

#[test]
fn test_mut_v() -> Result<(), CppMapError> {
    let mut list = SkipList::default();
    let _ = list.insert(1, 1.0_f32)?;
    let _ = list.insert(2, 2.0)?;
    let _ = list.insert(3, 3.0)?;
    let _ = list.insert(6, 6.1)?;

    let cursor = list.first();
    assert_eq!(list.get_v_at(cursor.unwrap()), Some(&1.0));

    let mut cursor = list.lower_bound(&2);
    cursor = list.next_pos(cursor);

    assert_eq!(list.get_v_at(cursor.unwrap()), Some(&3.0));
    list.set_v_at(cursor.unwrap(), 3.1);
    assert_eq!(list.get_v_at(cursor.unwrap()), Some(&3.1));

    if let Some(mut _kv) = list.get_mut_at(cursor.unwrap()) {
        *_kv.1 = 3.2;
    }
    assert_eq!(list.get_v_at(cursor.unwrap()), Some(&3.2));

    cursor = list.first();
    assert_eq!(list.get_v_at(cursor.unwrap()), Some(&1.0));
    cursor = list.last();
    assert_eq!(list.get_v_at(cursor.unwrap()), Some(&6.1));

    let cursor = list.lower_bound(&3);
    assert_eq!(list.get_v_at(cursor.unwrap()), Some(&3.2));
    let cursor = list.lower_bound(&1);
    assert_eq!(list.get_v_at(cursor.unwrap()), Some(&1.0));
    let cursor = list.lower_bound(&6);
    assert_eq!(list.get_v_at(cursor.unwrap()), Some(&6.1));
    // found key is greater than the given key
    let cursor = list.upper_bound(&6);
    assert!(cursor.is_none());

    let cursor = list.upper_bound(&1);
    assert_eq!(list.get_v_at(cursor.unwrap()), Some(&2.0));

    let cursor = list.upper_bound(&2);
    assert_eq!(list.get_v_at(cursor.unwrap()), Some(&3.2));
    let cursor = list.upper_bound(&3);
    assert_eq!(list.get_v_at(cursor.unwrap()), Some(&6.1));

    // found key is either equal to or greater than the given key
    let mut cursor = list.lower_bound(&6);
    assert_eq!(list.get_v_at(cursor.unwrap()), Some(&6.1));

    //list.debug_print();
    list.remove_by_index(&mut cursor);
    assert_eq!(list.get_v_at(cursor.unwrap()), Some(&3.2));
    let index = cursor.unwrap();

    assert_eq!(list.get_at(index), Some((&3, &3.2)));
    assert_eq!(list.get_k_at(index), Some(&3));
    assert_eq!(list.get_v_at(index), Some(&3.2));
    //let v = 3.2;
    assert_eq!(list.get_mut_at(index).unwrap().1, &3.2_f32);

    println!("{:?}", list);
    skp_test_head_and_tail_is_some(&list);
    Ok(())
}

#[test]
fn test_bounds_behavior() -> Result<(), CppMapError> {
    let mut list = SkipList::default();
    // Insert keys in non-sequential order to test proper ordering
    let _ = list.insert(5, 5.0)?;
    let _ = list.insert(1, 1.0)?;
    let _ = list.insert(3, 3.0)?;
    let _ = list.insert(8, 8.0)?;
    let _ = list.insert(6, 6.0)?;

    // Test lower_bound
    // Exact match exists
    let cursor = list.lower_bound(&3);
    assert!(list.is_pos_valid(cursor));
    assert!(!list.is_at_first(cursor));
    assert!(!list.is_at_last(cursor));
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&3));

    // No exact match - should find next higher (5)
    let cursor = list.lower_bound(&4);
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&5));

    // At beginning of list
    let cursor = list.lower_bound(&0);
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&1));

    // At end of list
    let cursor = list.lower_bound(&8);
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&8));

    // Beyond end of list
    let cursor = list.lower_bound(&9);
    assert!(cursor.is_none());

    // Test upper_bound
    // Exact match exists - should find next higher (5)
    let cursor = list.upper_bound(&3);
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&5));

    // No exact match - should find next higher (5)
    let cursor = list.upper_bound(&4);
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&5));

    // At beginning of list
    let cursor = list.upper_bound(&0);
    assert!(cursor.is_some());

    // At end of list
    let cursor = list.upper_bound(&8);
    assert!(cursor.is_none());

    // Between existing elements (between 6 and 8)
    let cursor = list.upper_bound(&7);
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&8));

    // Test empty list behavior
    let empty_list: SkipList<i32, f32> = SkipList::default();
    let cursor = empty_list.lower_bound(&1);
    assert!(cursor.is_none());
    let cursor = empty_list.upper_bound(&1);
    assert!(cursor.is_none());

    // Test with single element
    let mut single_list = SkipList::default();
    let _ = single_list.insert(10, 10.0)?;
    let cursor = single_list.lower_bound(&9);
    assert_eq!(single_list.get_k_at(cursor.unwrap()), Some(&10));
    let cursor = single_list.lower_bound(&10);
    assert_eq!(single_list.get_k_at(cursor.unwrap()), Some(&10));
    let cursor = single_list.lower_bound(&11);
    assert!(cursor.is_none());
    let cursor = single_list.upper_bound(&9);
    assert_eq!(single_list.get_k_at(cursor.unwrap()), Some(&10));
    let cursor = single_list.upper_bound(&10);
    assert!(cursor.is_none());
    Ok(())
}

#[test]
fn test_bounds_with_operations() -> Result<(), CppMapError> {
    let mut list = SkipList::default();
    let _ = list.insert(2_i32, 2.0_f32)?;
    let _ = list.insert(4, 4.0)?;
    let _ = list.insert(6, 6.0)?;
    let _ = list.insert(8, 8.0)?;

    // Test that bounds work after modifications
    let cursor = list.lower_bound(&5);
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&6));
    assert_eq!(list.get_v_at(cursor.unwrap()), Some(&6.0));

    // Test upper_bound after insertion
    let cursor = list.upper_bound(&5);
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&6));
    let _ = list.insert(5, 5.0)?;
    let cursor = list.upper_bound(&5);
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&6));

    // Test bounds after removal
    let mut cursor = list.lower_bound(&4);
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&4));
    list.remove_by_index(&mut cursor);
    let cursor = list.lower_bound(&4);
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&5));

    // use the Debug and Display methods
    let _ = format!("{}", list.first().unwrap());
    let _ = format!("{:?}", list.last());
    let _ = format!("{}", list.first().unwrap());
    let _ = format!("{:?}", list.first().unwrap());
    let _idx: usize = list.first().unwrap().into();

    let _cursor: usize = list.last().unwrap().into();

    skp_test_head_and_tail_is_some(&list);
    list.clear();
    let _ = format!("{:?}", list);
    Ok(())
}

#[test]
fn test_bounds_with_duplicate_behavior() -> Result<(), CppMapError> {
    // Even though our list doesn't allow duplicates, test how it behaves
    let mut list = SkipList::default();
    let _ = list.insert(1, 1.0)?;
    let _ = list.insert(1, 1.1)?; // This should not overwrite

    // Should find the existing (overwritten) entry
    let cursor = list.lower_bound(&1);
    assert_eq!(list.get_v_at(cursor.unwrap()), Some(&1.0));
    let cursor = list.upper_bound(&1);
    assert!(cursor.is_none());
    Ok(())
}

#[test]
fn test_remove_current_basic() -> Result<(), CppMapError> {
    let mut list = SkipList::default();
    let _ = list.insert(1, 1.0)?;
    let _ = list.insert(2, 2.0)?;
    let _ = list.insert(3, 3.0)?;

    // Remove middle element
    let mut cursor = list.lower_bound(&2);
    let removed = list.remove_by_index(&mut cursor);
    assert_eq!(removed.unwrap(), (2, 2.0));
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&1)); // Should move to prev
    assert_eq!(list.len(), 2);

    // Verify list integrity
    let cursor = list.lower_bound(&1);
    let cursor = list.next_pos(cursor);
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&3)); // 1->3 now
    Ok(())
}

#[test]
fn test_remove_current_edge_cases() -> Result<(), CppMapError> {
    // Test empty list
    let empty_list: SkipList<i32, f32> = SkipList::default();
    let cursor = empty_list.first();
    assert_eq!(cursor, None);

    // Test single element list
    let mut single_list = SkipList::default();
    let _ = single_list.insert(1, 1.0)?;
    let mut cursor = single_list.lower_bound(&1);
    let removed = single_list.remove_by_index(&mut cursor);
    assert_eq!(removed.unwrap(), (1, 1.0));
    assert!(!single_list.is_pos_valid(cursor)); // Cursor should be invalidated
    assert!(single_list.is_empty());

    // Test removing head
    let mut list = SkipList::default();
    let _ = list.insert(1, 1.0)?;
    let _ = list.insert(2, 2.0)?;
    let mut cursor = list.lower_bound(&1);
    let _removed = list.remove_by_index(&mut cursor);
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&2)); // Should move to next
    assert_eq!(list.len(), 1);

    // Test removing tail
    let mut list = SkipList::default();
    let _ = list.insert(1, 1.0)?;
    let _ = list.insert(2, 2.0)?;
    let mut cursor = list.lower_bound(&2);
    let _removed = list.remove_by_index(&mut cursor);
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&1)); // Should be invalid
    assert_eq!(list.len(), 1);

    // Verify cursor is properly invalidated when list becomes empty
    let mut list = SkipList::default();
    let _ = list.insert(1, 1.0)?;
    let mut cursor = list.lower_bound(&1);
    let _removed = list.remove_by_index(&mut cursor);
    assert!(!list.is_pos_valid(cursor));
    assert!(cursor.is_none());
    Ok(())
}

#[test]
fn test_remove_current_with_prev_next_behavior() -> Result<(), CppMapError> {
    let mut list = SkipList::default();
    let _ = list.insert(1, 1.0)?;
    let _ = list.insert(2, 2.0)?;
    let _ = list.insert(3, 3.0)?;

    // Remove middle element (has both prev and next)
    let mut cursor = list.lower_bound(&2);
    let _removed = list.remove_by_index(&mut cursor);
    // Should move to prev (1) according to implementation
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&1));
    let cursor = list.next_pos(cursor);
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&3));
    let mut cursor = list.prev_pos(cursor);

    // Remove again (now at head)
    let _removed = list.remove_by_index(&mut cursor);
    // Should move to next (3) since no prev exists
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&3));

    // Remove last remaining element
    let _removed = list.remove_by_index(&mut cursor);
    assert!(!list.is_pos_valid(cursor));
    assert!(list.is_empty());
    Ok(())
}

#[test]
fn test_remove_current_multiple_operations() -> Result<(), CppMapError> {
    let mut list = SkipList::default();
    for i in 1..=10_i32 {
        let _ = list.insert(i, i as f32)?;
    }

    // Remove in sequence
    let mut cursor = list.lower_bound(&5);
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&5));
    assert_eq!(list.get_v_at(cursor.unwrap()), Some(&5.0));

    // Should remove: 5 → 4 → 3 → 2 → 1
    for expected in [5, 4_i32, 3, 2, 1].iter() {
        let removed = list.remove_by_index(&mut cursor);
        assert_eq!(removed.unwrap().1, *expected as f32);
        assert_eq!(removed.unwrap().0, *expected);
        if *expected > 1 {
            assert_eq!(list.get_k_at(cursor.unwrap()).unwrap(), &(expected - 1));
        } else {
            assert_eq!(list.get_k_at(cursor.unwrap()).unwrap(), &(6));
        }
    }

    // Now at head, should move to next when removing
    let mut cursor = list.lower_bound(&5);
    assert_eq!(list.get_k_at(cursor.unwrap()), Some(&6));

    // Remove the rest
    while list.remove_by_index(&mut cursor).is_some() {}

    assert!(list.is_empty());
    assert!(!list.is_pos_valid(cursor));
    Ok(())
}
