// SPDX-License-Identifier: MIT OR Apache-2.0

// Copyright 2025 Eadf (github.com/eadf)
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use cpp_map::linkedlist::LinkedList;
use cpp_map::prelude::CppMapError;

mod common;
use common::*;

#[test]
fn test_new_list_is_empty() {
    let list: LinkedList<i32, String> = LinkedList::default();
    assert!(list.first().is_none());
    assert!(list.last().is_none());
    assert!(list.is_empty());
}

#[test]
fn test_insert_and_get() -> Result<(), CppMapError> {
    let mut list = LinkedList::default();
    let idx_1 = list.insert(1, "one")?;
    println!("{:?}", list);
    let _idx_2 = list.insert(2, "two")?;
    println!("{:?}", list);
    llt_test_head_and_tail_is_some(&list);
    let idx_2 = list.insert(2, "two")?;
    println!("{:?}", list);
    llt_test_head_and_tail_is_some(&list);
    let idx_4 = list.insert(4, "four")?;
    println!("{:?}", list);
    llt_test_head_and_tail_is_some(&list);

    assert_eq!(list.get(&1), Some(&"one"));
    assert_eq!(list.get(&2), Some(&"two"));
    assert_eq!(list.get(&4), Some(&"four"));
    assert_eq!(list.get_v_at(idx_1), Some(&"one"));
    assert_eq!(list.get_v_at(idx_2), Some(&"two"));
    assert_eq!(list.get_v_at(idx_4), Some(&"four"));
    let new_idx_2 = list.change_key_of_node(idx_2, 20).unwrap();
    assert_eq!(new_idx_2, idx_2);
    assert_eq!(list.get_v_at(idx_2), Some(&"two"));
    let new_idx_2 = list.change_key_of_node(idx_2, 2).unwrap();
    assert_eq!(new_idx_2, idx_2);
    assert_eq!(list.get_v_at(idx_2), Some(&"two"));
    assert_eq!(list.get(&2), Some(&"two"));
    assert_eq!(list.get(&3), None);
    Ok(())
}

#[test]
fn test_reinsert_node_same_key() -> Result<(), CppMapError> {
    let mut list = LinkedList::default();
    let idx = list.insert(1, "one")?;
    let _ = list.change_key_of_node(idx, 1).unwrap();
    assert_eq!(list.get(&1), Some(&"one"));
    Ok(())
}

#[test]
fn test_insert_duplicate_key() -> Result<(), CppMapError> {
    let mut list = LinkedList::default();
    list.insert(1, "one")?;
    list.insert(1, "new_one")?; // Should not overwrite
    llt_test_head_and_tail_is_some(&list);
    assert_eq!(list.get(&1), Some(&"one"));
    Ok(())
}

#[test]
fn test_string_keys() -> Result<(), CppMapError> {
    let mut list = LinkedList::<String, i32>::default();
    list.insert("one".to_string(), 1)?;
    list.insert("two".to_string(), 2)?;

    assert_eq!(list.get(&"one".to_string()), Some(&1));
    assert_eq!(list.remove(&"one".to_string()).unwrap().1, 1);
    assert_eq!(list.get(&"one".to_string()), None);
    llt_test_head_and_tail_is_some(&list);
    Ok(())
}
