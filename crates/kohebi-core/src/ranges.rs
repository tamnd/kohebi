//! Searching a sorted table of code point ranges.
//!
//! Both generated Unicode tables in this crate are the same shape, being a
//! list of inclusive ranges that is sorted and does not overlap, so both ask
//! the same question of it.

/// Whether `cp` falls in one of the ranges.
pub(crate) fn among(table: &[(u32, u32)], cp: u32) -> bool {
    table
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                std::cmp::Ordering::Greater
            } else if cp > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}
