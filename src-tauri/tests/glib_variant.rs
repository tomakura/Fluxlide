#![cfg(target_os = "linux")]

use glib::prelude::ToVariant;

#[test]
fn string_variant_iterators_write_their_out_pointer_in_optimized_builds() {
    let variant = ["one", "二", "three", "four"].to_variant();
    let mut iter = variant.array_iter_str().unwrap();
    assert_eq!(iter.next(), Some("one"));
    assert_eq!(iter.next_back(), Some("four"));
    assert_eq!(iter.nth(1), Some("three"));
    assert_eq!(iter.next(), None);
    assert_eq!(variant.array_iter_str().unwrap().nth_back(1), Some("three"));
    assert_eq!(variant.array_iter_str().unwrap().last(), Some("four"));
}
