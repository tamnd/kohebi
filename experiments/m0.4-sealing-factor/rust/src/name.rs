//! Interned names with a cached hash.
//!
//! The interpreter workload looks variables up by name on every step, so how
//! fast a name hashes decides a large part of the result. CPython interns
//! identifier-shaped string literals and caches each string's hash in the object,
//! so `env.vars[name]` is a load of a precomputed hash, one probe, and a pointer
//! comparison that succeeds. Hashing the bytes on every lookup, which is what a
//! plain `HashMap<String, _>` does, would have made the Rust side lose a race it
//! is not actually in.
//!
//! This is shared by every variant. It is not what sealing changes, so holding
//! it constant keeps the variants comparable to each other, and making it match
//! CPython keeps them comparable to CPython.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::rc::Rc;

pub struct NameData {
    hash: u64,
    pub text: Box<str>,
}

#[derive(Clone)]
pub struct Name(Rc<NameData>);

thread_local! {
    static INTERNED: RefCell<HashMap<Box<str>, Name>> = RefCell::new(HashMap::new());
}

impl Name {
    /// Interning happens once when the tree is built, as it does in CPython when
    /// the code object is created, so nothing on the hot path touches the table.
    pub fn new(text: &str) -> Name {
        INTERNED.with_borrow_mut(|table| {
            if let Some(existing) = table.get(text) {
                return existing.clone();
            }
            let name = Name(Rc::new(NameData {
                hash: fnv1a(text.as_bytes()),
                text: text.into(),
            }));
            table.insert(text.into(), name.clone());
            name
        })
    }

    #[inline(always)]
    pub fn as_str(&self) -> &str {
        &self.0.text
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

impl Hash for Name {
    #[inline(always)]
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.0.hash);
    }
}

impl PartialEq for Name {
    #[inline(always)]
    fn eq(&self, other: &Name) -> bool {
        // Interned, so identity settles it. The text comparison is here for the
        // same reason CPython keeps one: correctness does not depend on every
        // name having gone through the table.
        Rc::ptr_eq(&self.0, &other.0) || self.0.text == other.0.text
    }
}

impl Eq for Name {}

impl std::fmt::Debug for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

/// The hash is already computed and already well mixed, so the map should use it
/// rather than hash it again. This is what CPython's dict does.
#[derive(Default)]
pub struct PreHashed(u64);

impl Hasher for PreHashed {
    #[inline(always)]
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, _: &[u8]) {
        unreachable!("names hash through write_u64");
    }

    #[inline(always)]
    fn write_u64(&mut self, v: u64) {
        self.0 = v;
    }
}

pub type NameMap<V> = HashMap<Name, V, BuildHasherDefault<PreHashed>>;

pub fn name_map<V>() -> NameMap<V> {
    NameMap::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_text_interns_to_the_same_object() {
        let a = Name::new("total");
        let b = Name::new("total");
        assert!(Rc::ptr_eq(&a.0, &b.0));
        assert_eq!(a, b);
    }

    #[test]
    fn a_name_map_round_trips() {
        let mut m: NameMap<i64> = name_map();
        m.insert(Name::new("i"), 7);
        assert_eq!(m.get(&Name::new("i")), Some(&7));
        assert_eq!(m.get(&Name::new("j")), None);
    }
}
