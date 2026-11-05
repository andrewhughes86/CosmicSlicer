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

#[test]
fn test_insertion_positions() -> Result<(), Box<dyn std::error::Error>> {
    // Test basic sequential insertion
    for _ in 0..100 {
        let mut list = SkipList::default();

        // Insert first element
        let index_10 = list.insert(RoundedTen(10), "Ten")?;

        // Insert after first element
        let index_20 = list.insert_with_hint(RoundedTen(20), "Twenty", index_10)?;
        assert_eq!(*list.get_v_at(index_20).unwrap(), "Twenty");

        // Insert before first element
        let index_0 = list.insert_with_hint(RoundedTen(0), "Zero", index_10)?;
        assert_eq!(*list.get_v_at(index_0).unwrap(), "Zero");

        // Insert in middle with key duplicate -> nop
        let index_15 = list.insert_with_hint(RoundedTen(15), "Fifteen", index_20)?;
        assert_eq!(*list.get_v_at(index_15).unwrap(), "Ten");

        // Verify order
        let mut current = list.first();
        let mut values = Vec::new();
        while let Some((_key, value)) = list.get_at(current.unwrap()) {
            values.push(value.to_string());
            current = list.next_pos(current);
            if current.is_none() {
                break;
            }
        }
        assert_eq!(values, vec!["Zero", "Ten", "Twenty"]);
    }
    Ok(())
}

#[test]
fn test_hint_edge_cases() -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..100 {
        let mut list = SkipList::default();

        // Insert with hint at tail
        let index_100 = list.insert(RoundedTen(100), "Hundred")?;
        let index_50 = list.insert_with_hint(RoundedTen(50), "Fifty", index_100)?;
        assert_eq!(*list.get_v_at(index_50).unwrap(), "Fifty");

        // Insert with hint at head
        let index_25 = list.insert_with_hint(RoundedTen(25), "TwentyFive", index_50)?;
        assert_eq!(*list.get_v_at(index_25).unwrap(), "TwentyFive");

        // Insert with hint that's exactly correct
        let index_75 = list.insert_with_hint(RoundedTen(75), "SeventyFive", index_100)?;
        assert_eq!(*list.get_v_at(index_75).unwrap(), "SeventyFive");

        // Verify order
        let mut values = Vec::new();
        let mut current = list.first();
        while let Some((_, value)) = list.get_at(current.unwrap()) {
            values.push(value.to_string());
            current = list.next_pos(current);
            if current.is_none() {
                break;
            }
        }
        assert_eq!(
            values,
            vec!["TwentyFive", "Fifty", "SeventyFive", "Hundred"]
        );
    }
    Ok(())
}

#[test]
fn test_duplicate_keys() -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..100 {
        let mut list = SkipList::default();

        // Insert same key multiple times
        let index_5_1 = list.insert(RoundedTen(5), "Five1")?;
        let _index_5_2 = list.insert_with_hint(RoundedTen(5), "Five2", index_5_1)?;
        assert_eq!(index_5_1, _index_5_2);
    }
    Ok(())
}

#[test]
#[allow(clippy::unnecessary_literal_unwrap)]
fn test_insert_remove_sequence() -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..100 {
        let mut list = SkipList::default();

        let index_10 = list.insert(RoundedTen(10), "Ten")?;
        let mut _index_20 = Some(list.insert_with_hint(RoundedTen(20), "Twenty", index_10)?);
        let index_30 = Some(list.insert_with_hint(RoundedTen(30), "Thirty", _index_20.unwrap())?);

        // Remove middle element
        list.remove_by_index(&mut _index_20);

        // Reinsert with different hint
        _index_20 = Some(list.insert_with_hint(RoundedTen(20), "Twenty", index_30.unwrap())?);

        // Verify order
        let mut values = Vec::new();
        let mut current = list.first();
        while let Some((_, value)) = list.get_at(current.unwrap()) {
            values.push(value.to_string());
            current = list.next_pos(current);
            if current.is_none() {
                break;
            }
        }
        assert_eq!(values, vec!["Ten", "Twenty", "Thirty"]);
    }
    Ok(())
}

#[test]
fn test_extreme_values() -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..100 {
        let mut list = SkipList::default();

        // Insert minimum value
        let index_min = list.insert(RoundedTen(i32::MIN), "Min")?;

        // Insert maximum value
        let index_max = list.insert_with_hint(RoundedTen(i32::MAX), "Max", index_min)?;

        // Insert middle value
        let _index_mid = list.insert_with_hint(RoundedTen(0), "Mid", index_max)?;

        // Verify order
        let mut values = Vec::new();
        let mut current = list.first();
        while let Some((_, value)) = list.get_at(current.unwrap()) {
            values.push(value.to_string());
            current = list.next_pos(current);
            if current.is_none() {
                break;
            }
        }
        assert_eq!(values, vec!["Min", "Mid", "Max"]);
    }
    Ok(())
}
