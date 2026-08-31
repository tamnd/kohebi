//! What a `dict_keys`, a `dict_values` or a `dict_items` knows how to do.
//!
//! Which is nothing yet, and that is the point of the file. The three of them
//! have two public names between them, `isdisjoint` and `mapping`, and neither
//! is written. Without a table here, `d.keys().isdisjoint(other)` would be an
//! `AttributeError`, which says `dict_keys` has no such thing when it plainly
//! does. With one, it says the runtime has not got there yet, and a program
//! reading the message can tell those apart.
//!
//! `mapping` is an attribute rather than a method, and it lands here anyway
//! because attribute lookup falls through to this table when it finds nothing
//! else. That is the same route `str.format` takes.
//!
//! `dict_values` has only `mapping`. It is not set-like, because values need be
//! neither unique nor hashable, so there is nothing for `isdisjoint` to mean.

use super::Methods;
use crate::view::Of;

/// The table for one of the three, chosen by which view it is.
pub(super) fn methods(of: Of) -> &'static Methods {
    match of {
        Of::Keys | Of::Items => &SET_LIKE,
        Of::Values => &PLAIN,
    }
}

/// `dict_keys` and `dict_items`, which CPython treats as sets.
static SET_LIKE: Methods = Methods {
    ready: &[],
    later: &["isdisjoint", "mapping"],
};

/// `dict_values`, which it does not.
static PLAIN: Methods = Methods {
    ready: &[],
    later: &["mapping"],
};
