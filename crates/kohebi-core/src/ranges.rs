//! Searching a sorted table of code point ranges.
//!
//! Both generated Unicode tables in this crate are the same shape, being a
//! list of inclusive ranges that is sorted and does not overlap, so both ask
//! the same question of it.

/// Whether `cp` falls in one of the ranges.
pub(crate) fn among(table: &[(u32, u32)], cp: u32) -> bool {
    holding(table, cp).is_some()
}

/// The range `cp` falls in, for a caller that wants where in it the point sits
/// rather than only whether it is there.
///
/// [`crate::classify::decimal_value`] is the one such caller: a run of ten
/// decimal digits starts at its own zero, so the distance from the low end is
/// the digit.
pub(crate) fn holding(table: &[(u32, u32)], cp: u32) -> Option<(u32, u32)> {
    let at = table
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                std::cmp::Ordering::Greater
            } else if cp > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .ok()?;
    table.get(at).copied()
}
