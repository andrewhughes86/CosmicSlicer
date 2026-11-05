// SPDX-License-Identifier: MIT OR Apache-2.0

// Copyright 2025 Eadf (github.com/eadf)
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//use cpp_map::prelude::*;
//use std::cell::RefCell;
use std::cmp::Ordering;
//use std::rc::Rc;

/// A test _S_orted _O_bject that only sorts by key
#[derive(Debug, Clone)]
pub struct So {
    pub key: i32,
    pub value: i32,
}

impl Ord for So {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key.cmp(&other.key)
    }
}

#[allow(clippy::non_canonical_partial_ord_impl)]
impl PartialOrd for So {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.key.cmp(&other.key))
    }
}

impl Eq for So {}

impl PartialEq for So {
    // eq is only used for assert!, not sort insert
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.value == other.value
    }
}
impl So {
    pub fn new(key: i32, value: i32) -> Self {
        Self { key, value }
    }
}

/*
#[test]
fn linked_list_test6() -> Result<(), CppMapError> {
    let mut ll = LinkedList::<i8, i8>::default();
    let _ = ll.ordered_insert(5, 0)?; // 0
    assert_eq!(ll.iter().map(|(x, _)| *x).collect::<Vec<_>>(), vec![5_i8]);
    ll.clear();
    let _ = ll.ordered_insert(5, 0)?; // 0
    assert_eq!(ll.iter().map(|(x, _)| *x).collect::<Vec<_>>(), vec![5_i8]);
    Ok(())
}

#[test]
/// insert with the same key twice is a nop
fn linked_list_test7() -> Result<(), CppMapError> {
    let mut ll = LinkedList::<i8, i8>::default();
    let _ = ll.ordered_insert(5, 0)?; // 0
    let _ = ll.ordered_insert(5, 0)?; // 0
    assert_eq!(ll.iter().map(|(x, _)| *x).collect::<Vec<_>>(), vec![5_i8]);
    ll.clear();
    let _ = ll.ordered_insert(5, 0)?; // 0
    let _ = ll.ordered_insert(5, 1)?; // 0
    assert_eq!(ll.iter().map(|(x, _)| *x).collect::<Vec<_>>(), vec![5_i8]);
    Ok(())
}

#[test]
fn linked_list_test8() -> Result<(), CppMapError> {
    let mut ll = LinkedList::<i8, i8>::default();
    let _ = ll.ordered_insert(5, 0)?; // 0
    let v = ll.pop_front()?;
    assert_eq!(v, Some((5, 0)));
    let _ = ll.ordered_insert(1, 0)?; // 0
    let _ = ll.ordered_insert(2, 1)?; // 1
    let v = ll.pop_front()?;
    assert_eq!(v, Some((1, 0)));
    let v = ll.pop_front()?;
    assert_eq!(v, Some((2, 1)));
    assert_eq!(ll.len(), 0);
    ll.clear();
    let _ = ll.ordered_insert(5, 0)?; // 0
    let v = ll.pop_front()?;
    assert_eq!(v, Some((5, 0)));
    let _ = ll.ordered_insert(1, 0)?; // 0
    let _ = ll.ordered_insert(2, 1)?; // 1
    let v = ll.pop_front()?;
    assert_eq!(v, Some((1, 0)));
    let v = ll.pop_front()?;
    assert_eq!(v, Some((2, 1)));
    assert_eq!(ll.len(), 0);
    Ok(())
}

#[test]
fn linked_list_test9() -> Result<(), CppMapError> {
    let mut ll = LinkedList::<i8, i8>::default();
    let _ = ll.ordered_insert(5, 0)?;
    let v = ll.pop_front()?;
    assert_eq!(v, Some((5, 0)));
    let _ = ll.ordered_insert(1, 0)?;
    let _ = ll.ordered_insert(2, 1)?;
    let v = ll.pop_front()?;
    assert_eq!(v, Some((1, 0)));
    let v = ll.pop_front()?;
    assert_eq!(v, Some((2, 1)));
    let _ = ll.ordered_insert(5, 0)?;
    let _ = ll.ordered_insert(1, 1)?;
    let _ = ll.ordered_insert(2, 2)?;
    assert_eq!(
        ll.iter().map(|(x, _)| *x).collect::<Vec<_>>(),
        vec![1_i8, 2, 5]
    );
    ll.clear();
    let _ = ll.ordered_insert(5, 0)?; // 0
    let v = ll.pop_front()?;
    assert_eq!(v, Some((5, 0)));
    let _ = ll.ordered_insert(1, 0)?; // 0
    let _ = ll.ordered_insert(2, 1)?; // 1
    let v = ll.pop_front()?;
    assert_eq!(v, Some((1, 0)));
    let v = ll.pop_front()?;
    assert_eq!(v, Some((2, 1)));
    let _ = ll.ordered_insert(5, 0)?;
    let _ = ll.ordered_insert(1, 1)?;
    let _ = ll.ordered_insert(2, 2)?;
    assert_eq!(
        ll.iter().map(|(x, _)| *x).collect::<Vec<_>>(),
        vec![1_i8, 2, 5]
    );
    Ok(())
}

/// check that ordered insert handles duplicates correctly, insertion of duplicated keys should be
/// ignored.
#[test]
fn linked_list_test14() -> Result<(), CppMapError> {
    let mut ll = LinkedList::<So, i8>::default();
    let _ = ll.ordered_insert_pos(So::new(2, 1), 1, 0)?;
    let _ = ll.ordered_insert_pos(So::new(2, 0), 0, 1)?;
    assert_eq!(
        ll.iter().map(|(x, _)| x.clone()).collect::<Vec<_>>(),
        vec![So::new(2, 1)]
    );
    ll.clear();
    let _ = ll.ordered_insert_pos(So::new(2, 1), 1, 0)?;
    let _ = ll.ordered_insert_pos(So::new(2, 0), 0, 1)?;
    assert_eq!(
        ll.iter().map(|(x, _)| x.clone()).collect::<Vec<_>>(),
        vec![So::new(2, 1)]
    );

    let mut ll = LinkedList::<So, i8>::default();
    let _ = ll.ordered_insert_pos(So::new(1, 1), 1, 0)?;
    let _ = ll.ordered_insert_pos(So::new(2, 1), 1, 1)?;
    let _ = ll.ordered_insert_pos(So::new(2, 0), 0, 2)?;
    assert_eq!(
        ll.iter().map(|(x, _)| x.clone()).collect::<Vec<_>>(),
        vec![So::new(1, 1), So::new(2, 1)]
    );
    ll.clear();
    let _ = ll.ordered_insert_pos(So::new(1, 1), 1, 0)?;
    let _ = ll.ordered_insert_pos(So::new(2, 1), 1, 1)?;
    let _ = ll.ordered_insert_pos(So::new(2, 0), 0, 2)?;
    assert_eq!(
        ll.iter().map(|(x, _)| x.clone()).collect::<Vec<_>>(),
        vec![So::new(1, 1), So::new(2, 1)]
    );
    ll.clear();

    let _ = ll.ordered_insert_pos(So::new(1, 1), 1, 1)?;
    let _ = ll.ordered_insert_pos(So::new(2, 1), 1, 1)?;
    let _ = ll.ordered_insert_pos(So::new(2, 0), 0, 1)?;
    assert_eq!(
        ll.iter().map(|(x, _)| x.clone()).collect::<Vec<_>>(),
        vec![So::new(1, 1), So::new(2, 1)]
    );
    Ok(())
}

/// check that ordered insert handles duplicates correctly, insertion of duplicated keys should be
/// ignored.
#[test]
fn linked_list_test15() -> Result<(), CppMapError> {
    let mut ll = LinkedList::<So, i8>::default();
    let _ = ll.ordered_insert_pos(So::new(2, 0), 0, 0)?;
    let _ = ll.ordered_insert_pos(So::new(2, 1), 1, 1)?;
    let _ = ll.ordered_insert_pos(So::new(2, 2), 1, 0)?;
    assert_eq!(
        ll.iter().map(|(x, _)| x.clone()).collect::<Vec<_>>(),
        vec![So::new(2, 0)]
    );
    let _ = ll.ordered_insert_pos(So::new(2, -1), -1, 2)?;
    assert_eq!(
        ll.iter().map(|(x, _)| x.clone()).collect::<Vec<_>>(),
        vec![So::new(2, 0)]
    );
    Ok(())
}

/// check that ordered insert handles duplicates correctly, insertion of duplicated keys should be
/// ignored.
#[test]
fn linked_list_test16() -> Result<(), CppMapError> {
    let mut ll = LinkedList::<So, i8>::default();
    let _ = ll.ordered_insert_pos(So::new(2, 0), 0, ll.tail())?;
    let _ = ll.ordered_insert_pos(So::new(2, 1), 1, ll.tail())?;
    let _ = ll.ordered_insert_pos(So::new(2, 2), 1, ll.tail())?;
    assert_eq!(
        ll.iter().map(|(x, _)| x.clone()).collect::<Vec<_>>(),
        vec![So::new(2, 0)]
    );
    let _ = ll.ordered_insert_pos(So::new(2, -1), -1, ll.tail())?;
    assert_eq!(
        ll.iter().map(|(x, _)| x.clone()).collect::<Vec<_>>(),
        vec![So::new(2, 0)]
    );
    Ok(())
}

/// check that ordered insert handles duplicates correctly, insertion of duplicated keys should be
/// ignored.
#[test]
fn linked_list_test17() -> Result<(), CppMapError> {
    let mut ll = LinkedList::<So, i8>::default();
    let _ = ll.ordered_insert_pos(So::new(1, 0), 0, ll.tail())?;
    let _ = ll.ordered_insert_pos(So::new(2, 1), 1, ll.tail())?;
    let _ = ll.ordered_insert_pos(So::new(4, 4), 0, ll.tail())?;
    let _ = ll.ordered_insert_pos(So::new(2, 2), 1, ll.tail())?;
    assert_eq!(
        ll.iter().map(|(x, _)| x.clone()).collect::<Vec<_>>(),
        vec![So::new(1, 0), So::new(2, 1), So::new(4, 4),]
    );
    Ok(())
}

#[test]
/// test LinkedList::lower_bound()
fn linked_list_lower_bound_01() -> Result<(), CppMapError> {
    let mut ll = LinkedList::<i8, i8>::default();
    let _ = ll.ordered_insert(0, 0)?; // 0
    let _ = ll.ordered_insert(1, 1)?; // 1
    let _ = ll.ordered_insert(2, 2)?; // 2
    let _ = ll.ordered_insert(5, 3)?; // 3
    assert_eq!(
        ll.iter().map(|(x, _)| *x).collect::<Vec<_>>(),
        vec![0_i8, 1, 2, 5]
    );
    // Returns the first element in the container whose key is not considered to go
    // before position (i.e., either it is equivalent or goes after).
    // Returns None if no data is found
    let v = ll.lower_bound(0)?;
    assert_eq!(v, Some(0));
    let v = ll.lower_bound(1)?;
    assert_eq!(v, Some(1));
    let v = ll.lower_bound(2)?;
    assert_eq!(v, Some(2));
    let v = ll.lower_bound(5)?;
    assert_eq!(v, Some(3));
    let v = ll.lower_bound(15)?;
    assert_eq!(v, None);
    let v = ll.lower_bound(0)?;
    assert_eq!(v, Some(0));
    let v = ll.lower_bound(1)?;
    assert_eq!(v, Some(1));
    let v = ll.lower_bound(2)?;
    assert_eq!(v, Some(2));
    let v = ll.lower_bound(5)?;
    assert_eq!(v, Some(3));
    let v = ll.lower_bound(15)?;
    assert_eq!(v, None);
    Ok(())
}

#[test]
/// test lower_bound() when list is empty
fn linked_list_lower_bound_02() -> Result<(), CppMapError> {
    let ll = LinkedList::<i8, i8>::default();
    let v = ll.lower_bound(0)?;
    assert_eq!(v, None);
    Ok(())
}
*/

/*
#[test]
/// test PIterator::remove_current
fn linked_list_pointer_test01() -> Result<(), CppMapError> {
    let ll = Rc::from(RefCell::from(LinkedList::<i8, i8>::default()));
    let _ = ll.borrow_mut().ordered_insert(1, 0)?; // 0
    let _ = ll.borrow_mut().ordered_insert(2, 1)?; // 1
    let _ = ll.borrow_mut().ordered_insert(3, 2)?; // 2
    assert_eq!(
        ll.borrow().iter().map(|(x, _)| *x).collect::<Vec<_>>(),
        vec![1_i8, 2, 3]
    );

    let mut p = PIterator::new(Rc::clone(&ll))?;
    p.next()?;
    p.next()?;
    let v = p.get_k()?;
    assert_eq!(v, 3);
    let v = p.remove_current()?;
    assert_eq!(v, (3, 2));
    let v = p.remove_current()?;
    assert_eq!(v, (2, 1));
    let v = p.remove_current()?;
    assert_eq!(v, (1, 0));
    assert_eq!(ll.borrow().len(), 0);
    Ok(())
}

#[test]
/// test PIterator::lower_bound
fn linked_list_pointer_test02() -> Result<(), CppMapError> {
    let ll = Rc::from(RefCell::from(LinkedList::<i8, i8>::default()));
    let v = PIterator::lower_bound(Rc::clone(&ll), 1)?;
    assert!(!v.is_ok()?);
    let v = PIterator::lower_bound(Rc::clone(&ll), 1)?;
    assert!(!v.is_ok()?);
    Ok(())
}

#[test]
/// test PIterator::lower_bound
fn linked_list_pointer_test03() -> Result<(), CppMapError> {
    let ll = Rc::from(RefCell::from(LinkedList::<i8, i8>::default()));
    let _ = ll.borrow_mut().ordered_insert(1, 0)?; // 0
    let _ = ll.borrow_mut().ordered_insert(2, 1)?; // 1
    let _ = ll.borrow_mut().ordered_insert(3, 2)?; // 2
    let _ = ll.borrow_mut().ordered_insert(4, 3)?; // 3
    let lb = PIterator::lower_bound(Rc::clone(&ll), 3)?;
    assert!(lb.is_ok()?);
    assert_eq!(lb.get_k()?, 3);

    let lb = PIterator::lower_bound(Rc::clone(&ll), 1)?;
    assert!(lb.is_ok()?);
    assert_eq!(lb.get_k()?, 1);

    let v = PIterator::lower_bound(Rc::clone(&ll), 5)?;
    assert!(!v.is_ok()?);
    Ok(())
}

*/
