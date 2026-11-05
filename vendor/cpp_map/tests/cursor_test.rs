// SPDX-License-Identifier: MIT OR Apache-2.0

// Copyright 2025 Eadf (github.com/eadf)
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//use cpp_map::prelude::*;
mod common;
use common::*;

#[test]
fn test_forward_cursor() {
    let (list, map) = llt_create_test_data(500, 20, 7);

    // Compare forward iteration
    let mut lc = list.first();
    let mut list_items = Vec::new();
    list_items.push(lc);
    while let Some(v) = list.next_pos(lc) {
        list_items.push(Some(v));
        lc = Some(v);
    }
    let map_items: Vec<_> = map.iter().collect();
    assert_eq!(list_items.len(), map_items.len());

    for (idx, (m_k, m_v)) in list_items.iter().zip(map_items.iter()) {
        let kv = list.get_at(idx.unwrap());
        assert_eq!(kv, Some((*m_k, *m_v)));
    }
}

/*
#[test]
fn test_reverse_cursor() {
    let (list, map) = create_test_data(500, 30, 8);

    // Compare reverse iteration
    let mut lc: Cursor<_, _> = list.cursor_from_tail();
    let mut list_items = Vec::new();
    list_items.push(lc.current().unwrap());
    while let Some(v) = lc.prev(&list) {
        list_items.push(v);
    }
    let map_items: Vec<_> = map.iter().rev().collect();

    assert_eq!(list_items.len(), map_items.len());

    for (idx, (m_k, m_v)) in list_items.iter().zip(map_items.iter()) {
        let kv = list.peek_kv(*idx);
        assert_eq!(kv, Some((*m_k, *m_v)));
    }
}

#[test]
fn test_cursor_from() {
    let (list, map) = create_test_data(500, 40, 9);
    let (start_key, _) = map.iter().take(200).next().unwrap();

    // Get cursor starting from key
    let mut lc = list.cursor_at(start_key).unwrap();
    assert!(lc.is_valid());
    let mut list_items = Vec::new();
    list_items.push(lc.peek_kv(&list).unwrap());
    while let Some(v) = lc.next_kv(&list) {
        list_items.push(v);
    }

    // Get equivalent items from BTreeMap
    let map_items: Vec<_> = map.range(start_key..).collect();

    assert_eq!(list_items.len(), map_items.len());

    for ((l_k, l_v), (m_k, m_v)) in list_items.iter().zip(map_items.iter()) {
        assert_eq!(l_k, m_k);
        assert_eq!(l_v, m_v);
    }
}

#[test]
fn test_cursor_rev_from() {
    let (list, map) = create_test_data(500, 50, 10);
    let (start_key, _) = map.iter().take(300).next().unwrap();

    // Get reverse cursor starting from key
    let mut lc = list.cursor_at(start_key).unwrap();
    assert!(lc.is_valid());
    let mut list_items = Vec::new();
    list_items.push(lc.current().unwrap());
    while let Some(ni) = lc.prev(&list) {
        assert_eq!(list.peek_v(ni), lc.peek_v(&list));
        assert_eq!(list.peek_k(ni), lc.peek_k(&list));
        let k = lc.peek_k(&list).unwrap();
        assert_eq!(list.lookup(k).unwrap(), lc.peek_v(&list).unwrap());
        list_items.push(ni);
    }

    // Get equivalent items from BTreeMap (need to handle ranges differently for reverse)
    let map_items: Vec<_> = map.range(..=start_key).rev().collect();

    assert_eq!(list_items.len(), map_items.len());

    for (idx, (m_k, m_v)) in list_items.iter().zip(map_items.iter()) {
        let kv = list.peek_kv(*idx);
        assert_eq!(kv, Some((*m_k, *m_v)));
    }
}

#[test]
fn test_empty_cursors() {
    let list: BTreeLinkedList<i32, String> = BTreeLinkedList::default();

    assert!(!list.cursor_from_head().is_valid());
    assert!(!list.cursor_from_tail().is_valid());
    assert!(list.cursor_at(&0).is_none());
}

#[test]
fn test_mut_v() -> Result<(), CppMapError> {
    let mut list = BTreeLinkedList::default();
    let _ = list.insert(1, 1.0_f32)?;
    let _ = list.insert(2, 2.0)?;
    let _ = list.insert(3, 3.0)?;
    let _ = list.insert(6, 6.1)?;

    let cursor = list.cursor_from_head();
    assert_eq!(cursor.peek_v(&list), Some(&1.0));

    let mut cursor = list.cursor_at(&2).unwrap();
    cursor.next(&list).unwrap();

    assert_eq!(cursor.peek_v(&list), Some(&3.0));
    cursor.set_v(&mut list, 3.1);
    assert_eq!(cursor.peek_v(&list), Some(&3.1));

    if let Some(_v) = cursor.mut_v(&mut list) {
        *_v = 3.2;
    }
    assert_eq!(cursor.peek_v(&list), Some(&3.2));

    cursor.reset_to_head(&list);
    assert_eq!(cursor.peek_v(&list), Some(&1.0));
    cursor.reset_to_tail(&list);
    assert_eq!(cursor.peek_v(&list), Some(&6.1));

    let cursor = list.cursor_at(&3).unwrap();
    assert_eq!(cursor.peek_v(&list), Some(&3.2));
    let cursor = list.cursor_at(&1).unwrap();
    assert_eq!(cursor.peek_v(&list), Some(&1.0));
    let cursor = list.cursor_at(&6).unwrap();
    assert_eq!(cursor.peek_v(&list), Some(&6.1));
    // found key is greater than the given key
    let cursor = list.upper_bound_by_k(&6);
    assert_eq!(cursor.peek_v(&list), None);
    // found key is either equal to or greater than the given key
    let cursor = list.lower_bound_by_k(&6);
    assert_eq!(cursor.peek_v(&list), Some(&6.1));

    list.remove_by_index(cursor.current().unwrap());
    assert_eq!(cursor.peek_v(&list), None);
    let index = cursor.current().unwrap();

    assert_eq!(list.peek_k(index), None);
    assert_eq!(list.peek_v(index), None);
    assert_eq!(list.peek_kv(index), None);
    assert_eq!(cursor.peek_k(&list), None);
    assert_eq!(cursor.peek_v(&list), None);
    assert_eq!(cursor.mut_v(&mut list), None);

    println!("{:?}", list);
    test_head_and_tail_is_some(&list);
    Ok(())
}

#[test]
fn test_bounds_behavior() -> Result<(), CppMapError> {
    let mut list = BTreeLinkedList::default();
    // Insert keys in non-sequential order to test proper ordering
    let _ = list.insert(5, 5.0)?;
    let _ = list.insert(1, 1.0)?;
    let _ = list.insert(3, 3.0)?;
    let _ = list.insert(8, 8.0)?;
    let _ = list.insert(6, 6.0)?;

    // Test lower_bound
    // Exact match exists
    let cursor = list.lower_bound_by_k(&3);
    assert!(cursor.is_really_valid(&list));
    assert!(!cursor.is_at_tail(&list));
    assert!(!cursor.is_at_head(&list));
    assert_eq!(cursor.peek_k(&list), Some(&3));

    // No exact match - should find next higher (5)
    let cursor = list.lower_bound_by_k(&4);
    assert_eq!(cursor.peek_k(&list), Some(&5));

    // At beginning of list
    let cursor = list.lower_bound_by_k(&0);
    assert_eq!(cursor.peek_k(&list), Some(&1));

    // At end of list
    let cursor = list.lower_bound_by_k(&8);
    assert_eq!(cursor.peek_k(&list), Some(&8));

    // Beyond end of list
    let cursor = list.lower_bound_by_k(&9);
    assert_eq!(cursor.peek_k(&list), None);

    // Test upper_bound
    // Exact match exists - should find next higher (5)
    let cursor = list.upper_bound_by_k(&3);
    assert_eq!(cursor.peek_k(&list), Some(&5));

    // No exact match - should find next higher (5)
    let cursor = list.upper_bound_by_k(&4);
    assert_eq!(cursor.peek_k(&list), Some(&5));

    // At beginning of list
    let cursor = list.upper_bound_by_k(&0);
    assert_eq!(cursor.peek_k(&list), Some(&1));

    // At end of list
    let cursor = list.upper_bound_by_k(&8);
    assert_eq!(cursor.peek_k(&list), None);

    // Between existing elements (between 6 and 8)
    let cursor = list.upper_bound_by_k(&7);
    assert_eq!(cursor.peek_k(&list), Some(&8));

    // Test empty list behavior
    let empty_list: BTreeLinkedList<i32, f32> = BTreeLinkedList::default();
    let cursor = empty_list.lower_bound_by_k(&1);
    assert_eq!(cursor.peek_k(&empty_list), None);
    let cursor = empty_list.upper_bound_by_k(&1);
    assert_eq!(cursor.peek_k(&empty_list), None);

    // Test with single element
    let mut single_list = BTreeLinkedList::default();
    let _ = single_list.insert(10, 10.0)?;
    let cursor = single_list.lower_bound_by_k(&9);
    assert_eq!(cursor.peek_k(&single_list), Some(&10));
    let cursor = single_list.lower_bound_by_k(&10);
    assert_eq!(cursor.peek_k(&single_list), Some(&10));
    let cursor = single_list.lower_bound_by_k(&11);
    assert_eq!(cursor.peek_k(&single_list), None);
    let cursor = single_list.upper_bound_by_k(&9);
    assert_eq!(cursor.peek_k(&single_list), Some(&10));
    let cursor = single_list.upper_bound_by_k(&10);
    assert_eq!(cursor.peek_k(&single_list), None);
    Ok(())
}

#[test]
fn test_bounds_with_operations() -> Result<(), CppMapError> {
    let mut list = BTreeLinkedList::default();
    let _ = list.insert(2_i32, 2.0_f32)?;
    let _ = list.insert(4, 4.0)?;
    let _ = list.insert(6, 6.0)?;
    let _ = list.insert(8, 8.0)?;

    // Test that bounds work after modifications
    let cursor = list.lower_bound_by_k(&5);
    assert_eq!(cursor.peek_k(&list), Some(&6));
    cursor.set_v(&mut list, 6.5);
    assert_eq!(cursor.peek_v(&list), Some(&6.5));

    // Test upper_bound after insertion
    let cursor = list.upper_bound_by_k(&5);
    assert_eq!(cursor.peek_k(&list), Some(&6));
    let _ = list.insert(5, 5.0)?;
    let cursor = list.upper_bound_by_k(&5);
    assert_eq!(cursor.peek_k(&list), Some(&6));

    // Test bounds after removal
    let cursor = list.lower_bound_by_k(&4);
    assert_eq!(cursor.peek_k(&list), Some(&4));
    list.remove_by_index(cursor.current().unwrap());
    let cursor = list.lower_bound_by_k(&4);
    assert_eq!(cursor.peek_k(&list), Some(&5));

    // use the Debug and Display methods
    let _ = format!("{}", list.cursor_from_head());
    let _ = format!("{:?}", list.cursor_from_head());
    let _ = format!("{}", list.cursor_from_head().current().unwrap());
    let _ = format!("{:?}", list.cursor_from_head().current().unwrap());
    let _idx: usize = list.cursor_from_head().current().unwrap().into();

    let _cursor: Cursor<i32, f32> = list.cursor_from_tail().current().unwrap().into();

    test_head_and_tail_is_some(&list);
    list.clear();
    let _ = format!("{:?}", list);
    Ok(())
}

#[test]
fn test_bounds_with_duplicate_behavior() -> Result<(), CppMapError> {
    // Even though our list doesn't allow duplicates, test how it behaves
    let mut list = BTreeLinkedList::default();
    let _ = list.insert(1, 1.0)?;
    let _ = list.insert(1, 1.1)?; // This should not overwrite

    // Should find the existing (overwritten) entry
    let cursor = list.lower_bound_by_k(&1);
    assert_eq!(cursor.peek_v(&list), Some(&1.0));
    let cursor = list.upper_bound_by_k(&1);
    assert_eq!(cursor.peek_k(&list), None);
    Ok(())
}

#[test]
fn test_remove_current_basic() -> Result<(), CppMapError> {
    let mut list = BTreeLinkedList::default();
    let _ = list.insert(1, 1.0)?;
    let _ = list.insert(2, 2.0)?;
    let _ = list.insert(3, 3.0)?;

    // Remove middle element
    let mut cursor = list.cursor_at(&2).unwrap();
    let removed = cursor.remove_current(&mut list).unwrap();
    assert_eq!(removed, (2, 2.0));
    assert_eq!(cursor.peek_k(&list), Some(&1)); // Should move to prev
    assert_eq!(list.len(), 2);

    // Verify list integrity
    let cursor = list.cursor_at(&1).unwrap().move_next(&list);
    assert_eq!(cursor.peek_k(&list), Some(&3)); // 1->3 now
    Ok(())
}
*/

/*
#[test]
fn test_remove_current_edge_cases() -> Result<(), CppMapError> {
    // Test empty list
    let mut empty_list: BTreeLinkedList<i32, f32> = BTreeLinkedList::default();
    let mut cursor = empty_list.cursor_from_head();
    assert_eq!(cursor.remove_current(&mut empty_list), None);

    // Test single element list
    let mut single_list = BTreeLinkedList::default();
    let _ = single_list.insert(1, 1.0)?;
    let mut cursor = single_list.cursor_at(&1).unwrap();
    let removed = cursor.remove_current(&mut single_list).unwrap();
    assert_eq!(removed, (1, 1.0));
    assert!(!cursor.is_valid()); // Cursor should be invalidated
    assert!(single_list.is_empty());

    // Test removing head
    let mut list = BTreeLinkedList::default();
    let _ = list.insert(1, 1.0)?;
    let _ = list.insert(2, 2.0)?;
    let mut cursor = list.cursor_at(&1).unwrap();
    cursor.remove_current(&mut list);
    assert_eq!(cursor.peek_k(&list), Some(&2)); // Should move to next
    assert_eq!(list.len(), 1);

    // Test removing tail
    let mut list = BTreeLinkedList::default();
    let _ = list.insert(1, 1.0)?;
    let _ = list.insert(2, 2.0)?;
    let mut cursor = list.cursor_at(&2).unwrap();
    cursor.remove_current(&mut list);
    assert_eq!(cursor.peek_k(&list), Some(&1)); // Should be invalid
    assert_eq!(list.len(), 1);

    // Verify cursor is properly invalidated when list becomes empty
    let mut list = BTreeLinkedList::default();
    let _ = list.insert(1, 1.0)?;
    let mut cursor = list.cursor_at(&1).unwrap();
    cursor.remove_current(&mut list);
    assert!(!cursor.is_valid());
    assert!(cursor.peek_k(&list).is_none());
    Ok(())
}

#[test]
fn test_remove_current_with_prev_next_behavior() -> Result<(), CppMapError> {
    let mut list = BTreeLinkedList::default();
    let _ = list.insert(1, 1.0)?;
    let _ = list.insert(2, 2.0)?;
    let _ = list.insert(3, 3.0)?;

    // Remove middle element (has both prev and next)
    let mut cursor = list.cursor_at(&2).unwrap();
    cursor.remove_current(&mut list);
    // Should move to prev (1) according to implementation
    assert_eq!(cursor.peek_k(&list), Some(&1));
    let cursor = cursor.move_next(&list);
    assert_eq!(cursor.peek_k(&list), Some(&3));
    let mut cursor = cursor.move_prev(&list);

    // Remove again (now at head)
    cursor.remove_current(&mut list);
    // Should move to next (3) since no prev exists
    assert_eq!(cursor.peek_k(&list), Some(&3));

    // Remove last remaining element
    cursor.remove_current(&mut list);
    assert!(!cursor.is_valid());
    assert!(list.is_empty());
    Ok(())
}
*/

/*
#[test]
fn test_remove_current_multiple_operations() -> Result<(), CppMapError> {
    let mut list = BTreeLinkedList::default();
    for i in 1..=10 {
        let _ = list.insert(i, i as f32)?;
    }

    // Remove in sequence
    let mut cursor = list.cursor_at(&5).unwrap();
    for expected in [4, 3, 2, 1].iter() {
        cursor.remove_current(&mut list);
        assert_eq!(cursor.peek_k(&list), Some(expected));
    }

    // Now at head, should move to next when removing
    cursor.remove_current(&mut list);
    assert_eq!(cursor.peek_k(&list), Some(&6));

    // Remove rest
    while cursor.remove_current(&mut list).is_some() {
        // Just emptying the list
    }

    assert!(list.is_empty());
    assert!(!cursor.is_valid());
    Ok(())
}
*/
