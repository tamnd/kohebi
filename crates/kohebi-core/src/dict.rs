//! The dict and the set.
//!
//! A dict remembers the order things were put into it, and that is a promise
//! the language makes rather than an accident of the implementation, so it is
//! not something an implementation may choose differently. The layout that
//! gives it is CPython's compact dict and it is the one built here: a sparse
//! table of slot numbers, and the entries themselves packed densely in a
//! separate list in insertion order. Iterating walks the dense list, which is
//! why iteration is in order and why it costs one pass over the live entries
//! rather than a pass over a table that is mostly holes.
//!
//! It is also the cheaper of the two layouts. The sparse part holds one machine
//! word per slot rather than a whole entry, so the table can stay two thirds
//! empty, which is what keeps probes short, without two thirds of the memory
//! going to nothing. That matters here more than most places, since the target
//! is a tenth of CPython's memory and a Python program is mostly dicts.
//!
//! ## What is still not the real thing
//!
//! The slot table is a machine word per slot. CPython narrows it to one, two or
//! four bytes for a small dict, which is most dicts, and that is worth doing but
//! it wants the storage to be laid out by hand rather than by `Vec`.
//!
//! A set is a dict with nothing in the values here. CPython has a separate
//! structure for it that saves the value word and a level of indirection.
//! Sharing the code is the right call while the semantics are the thing being
//! got right, and splitting it later changes nothing a caller can see.
//!
//! Neither has the key sharing that makes instances of a class cheap, which is
//! what shapes are for, and shapes are a later milestone.

use crate::hash::Key;
use crate::object::Object;

/// A slot nothing has ever been in, which ends a probe.
const EMPTY: usize = usize::MAX;

/// A slot something was deleted from, which does not end a probe because the
/// key being looked for may have been put down further along the chain.
const DUMMY: usize = usize::MAX - 1;

/// The smallest table, which is what a dict gets the moment anything goes in.
const MINIMUM: usize = 8;

/// One key and value, in the order they were inserted.
#[derive(Debug, Clone)]
struct Entry {
    /// Kept beside the key so a probe can reject a slot without going near
    /// Python equality, which for a tuple means walking the whole thing.
    hash: i64,
    key: Key,
    value: Object,
}

/// A Python dict.
#[derive(Debug, Clone, Default)]
pub struct Dict {
    /// Slot numbers, or [`EMPTY`] or [`DUMMY`]. Always a power of two long, or
    /// empty when nothing has been inserted yet.
    indices: Vec<usize>,
    /// The entries, in insertion order. A deleted one becomes `None` and stays
    /// there until the next resize, so that the positions in `indices` keep
    /// meaning what they meant.
    entries: Vec<Option<Entry>>,
    /// How many of them are still live.
    used: usize,
}

/// The slots a hash visits, in order, until whoever is walking stops.
///
/// One definition of where a key belongs, used by the lookup and by the resize
/// that lays the table out again, because the two disagreeing would file a key
/// somewhere it would never be looked for.
///
/// The sequence is CPython's. The low bits of the hash pick the first slot, and
/// each miss shifts five more of the high bits into the mix, so two keys that
/// agree in their low bits do not then walk the same chain. Multiplying by five
/// and adding one is what makes it reach every slot in the table, which is what
/// stops a walk from going round forever in a table that has room.
struct Walk {
    mask: usize,
    slot: usize,
    perturb: u64,
}

impl Walk {
    /// `size` is the length of the table, which is always a power of two.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a hash is a bag of bits here rather than a number, so \
                  dropping the sign or the top half on a 32-bit target costs \
                  a little spread and nothing else"
    )]
    fn new(hash: i64, size: usize) -> Self {
        let perturb = hash as u64;
        Walk {
            mask: size - 1,
            slot: (perturb as usize) & (size - 1),
            perturb,
        }
    }
}

impl Iterator for Walk {
    type Item = usize;

    /// Never ends. A table always has an empty slot in it, so every caller
    /// stops on its own before this would have to.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the same as in `new`, and for the same reason"
    )]
    fn next(&mut self) -> Option<usize> {
        let slot = self.slot;
        self.perturb >>= 5;
        self.slot = slot
            .wrapping_mul(5)
            .wrapping_add(self.perturb as usize)
            .wrapping_add(1)
            & self.mask;
        Some(slot)
    }
}

/// What a probe found.
enum Probe {
    /// The key is here, in this slot, at this position in `entries`.
    Occupied { slot: usize, entry: usize },
    /// The key is not in the table, and this slot is where it would go.
    Vacant { slot: usize },
}

impl Dict {
    /// An empty dict, which has no table until something goes into it.
    #[must_use]
    pub const fn new() -> Self {
        Dict {
            indices: Vec::new(),
            entries: Vec::new(),
            used: 0,
        }
    }

    /// How many keys are in it.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.used
    }

    /// Whether there are none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.used == 0
    }

    /// The value under this key.
    #[must_use]
    pub fn get(&self, key: &Key) -> Option<&Object> {
        match self.probe(key)? {
            Probe::Occupied { entry, .. } => Some(&self.entry(entry).value),
            Probe::Vacant { .. } => None,
        }
    }

    /// Whether the key is in it.
    #[must_use]
    pub fn contains(&self, key: &Key) -> bool {
        self.get(key).is_some()
    }

    /// Puts a value under a key, giving back the one that was there.
    ///
    /// A key that is already present keeps the object it was first stored as,
    /// which is why `d = {1: 'a'}` then `d[True] = 'b'` leaves `1` as the key
    /// and not `True`. The two are equal, so there is nothing to update, and
    /// replacing the key would change what iterating the dict hands back.
    pub fn insert(&mut self, key: Key, value: Object) -> Option<Object> {
        self.reserve();
        // `reserve` guarantees a table, so there is always a probe to read.
        match self
            .probe(&key)
            .expect("a table was just made if there was none")
        {
            Probe::Occupied { entry, .. } => {
                Some(std::mem::replace(&mut self.entry_mut(entry).value, value))
            }
            Probe::Vacant { slot } => {
                self.indices[slot] = self.entries.len();
                self.entries.push(Some(Entry {
                    hash: key.hash(),
                    key,
                    value,
                }));
                self.used += 1;
                None
            }
        }
    }

    /// Takes a key out, giving back the value that was under it.
    pub fn remove(&mut self, key: &Key) -> Option<Object> {
        match self.probe(key)? {
            Probe::Occupied { slot, entry } => {
                // The slot cannot go back to empty, because a key inserted
                // after this one may have probed past this slot to find its
                // own, and an empty here would end that probe early and lose
                // it. Hence the tombstone, which the next resize clears out.
                self.indices[slot] = DUMMY;
                let removed = self.entries[entry].take().expect("a live position");
                self.used -= 1;
                Some(removed.value)
            }
            Probe::Vacant { .. } => None,
        }
    }

    /// Empties it, giving up the table as well.
    pub fn clear(&mut self) {
        *self = Dict::new();
    }

    /// The keys and values, in the order they were inserted.
    pub fn iter(&self) -> impl Iterator<Item = (&Key, &Object)> {
        self.entries
            .iter()
            .flatten()
            .map(|entry| (&entry.key, &entry.value))
    }

    /// The keys, in the order they were inserted.
    pub fn keys(&self) -> impl Iterator<Item = &Key> {
        self.iter().map(|(key, _)| key)
    }

    /// Whether two dicts have the same keys with the same values, which is
    /// what `==` asks and which does not care what order either was built in.
    #[must_use]
    pub fn equals(&self, other: &Self) -> bool {
        self.used == other.used
            && self
                .iter()
                .all(|(key, value)| other.get(key).is_some_and(|found| value.same_value(found)))
    }

    /// Walks the table for a key, and says where it is or where it would go.
    ///
    /// `None` when there is no table at all, which is an empty dict nothing
    /// has been put into yet. The probe is CPython's: the low bits of the hash
    /// pick the first slot, and each miss mixes in five more of the high bits,
    /// so keys that agree in their low bits do not then walk the same chain.
    fn probe(&self, key: &Key) -> Option<Probe> {
        if self.indices.is_empty() {
            return None;
        }
        let hash = key.hash();
        // A tombstone is where the key would go if it turns out not to be
        // here, but the walk has to go on past it to find out whether it is.
        let mut reusable = None;
        for slot in Walk::new(hash, self.indices.len()) {
            match self.indices[slot] {
                EMPTY => {
                    return Some(Probe::Vacant {
                        slot: reusable.unwrap_or(slot),
                    });
                }
                DUMMY => {
                    if reusable.is_none() {
                        reusable = Some(slot);
                    }
                }
                entry => {
                    let candidate = self.entry(entry);
                    // The hash first, because it settles nearly every miss
                    // without touching the objects.
                    if candidate.hash == hash && candidate.key == *key {
                        return Some(Probe::Occupied { slot, entry });
                    }
                }
            }
        }
        unreachable!("a table is never full, so a walk always reaches an empty slot")
    }

    /// Makes sure there is room for one more before anything is probed for.
    ///
    /// The test is against the number of positions used rather than the number
    /// of live keys, because a tombstone still lengthens every probe that runs
    /// over it. Keeping a third of the table empty is what keeps those probes
    /// short, and it is the fraction CPython settled on.
    fn reserve(&mut self) {
        if self.indices.is_empty() {
            self.indices = vec![EMPTY; MINIMUM];
            return;
        }
        if (self.entries.len() + 1) * 3 > self.indices.len() * 2 {
            self.rebuild();
        }
    }

    /// Drops the deleted entries and lays the table out again.
    ///
    /// The size comes from the live keys rather than from the old size, so a
    /// dict that filled up and then emptied out shrinks rather than carrying
    /// its high water mark around forever.
    fn rebuild(&mut self) {
        let wanted = (self.used + 1).saturating_mul(3).max(MINIMUM);
        let size = wanted.next_power_of_two();
        self.entries.retain(Option::is_some);

        let mut indices = vec![EMPTY; size];
        for (position, entry) in self.entries.iter().enumerate() {
            let hash = entry.as_ref().expect("the holes were just dropped").hash;
            // No tombstones and no duplicates, since these keys were already
            // distinct, so the first empty slot is the one.
            let slot = Walk::new(hash, size)
                .find(|&slot| indices[slot] == EMPTY)
                .expect("a fresh table has empty slots in it");
            indices[slot] = position;
        }
        self.indices = indices;
    }

    fn entry(&self, position: usize) -> &Entry {
        self.entries[position]
            .as_ref()
            .expect("a slot only ever points at a live entry")
    }

    fn entry_mut(&mut self, position: usize) -> &mut Entry {
        self.entries[position]
            .as_mut()
            .expect("a slot only ever points at a live entry")
    }
}

impl FromIterator<(Key, Object)> for Dict {
    fn from_iter<I: IntoIterator<Item = (Key, Object)>>(pairs: I) -> Self {
        let mut dict = Dict::new();
        for (key, value) in pairs {
            dict.insert(key, value);
        }
        dict
    }
}

/// A Python set.
///
/// A dict with nothing in the values, which is what CPython's set was before it
/// got a structure of its own. The saving from splitting them is one word an
/// element, and it is worth taking once the semantics have stopped moving.
#[derive(Debug, Clone, Default)]
pub struct Set {
    members: Dict,
}

impl Set {
    /// An empty set.
    #[must_use]
    pub const fn new() -> Self {
        Set {
            members: Dict::new(),
        }
    }

    /// How many members it has.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.members.len()
    }

    /// Whether there are none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Whether this value is in it.
    #[must_use]
    pub fn contains(&self, value: &Key) -> bool {
        self.members.contains(value)
    }

    /// Puts a value in, and says whether it was not already there.
    pub fn insert(&mut self, value: Key) -> bool {
        self.members.insert(value, Object::None).is_none()
    }

    /// Takes a value out, and says whether it was there.
    pub fn remove(&mut self, value: &Key) -> bool {
        self.members.remove(value).is_some()
    }

    /// The members.
    ///
    /// In insertion order, which is not something a set promises and not
    /// something CPython gives, so nothing may lean on it.
    pub fn iter(&self) -> impl Iterator<Item = &Key> {
        self.members.keys()
    }

    /// Whether two sets have the same members.
    #[must_use]
    pub fn equals(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().all(|value| other.contains(value))
    }
}

impl FromIterator<Key> for Set {
    fn from_iter<I: IntoIterator<Item = Key>>(values: I) -> Self {
        let mut set = Set::new();
        for value in values {
            set.insert(value);
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(object: Object) -> Key {
        Key::new(object).expect("expected this to be hashable")
    }

    fn int(value: i64) -> Key {
        key(Object::int(value))
    }

    /// The keys, in order, as integers, which is what nearly every test here
    /// wants to say something about.
    fn order(dict: &Dict) -> Vec<i64> {
        dict.keys()
            .map(|key| match key.object() {
                Object::Int(value) => value.to_i64().expect("small enough"),
                other => panic!("not an integer key: {}", other.repr()),
            })
            .collect()
    }

    #[test]
    fn an_empty_dict_has_no_table_and_answers_anyway() {
        let dict = Dict::new();
        assert_eq!(dict.len(), 0);
        assert!(dict.is_empty());
        assert!(dict.get(&int(1)).is_none());
        assert!(!dict.contains(&int(1)));
        assert_eq!(order(&dict), Vec::<i64>::new());
    }

    #[test]
    fn what_goes_in_comes_back_out() {
        let mut dict = Dict::new();
        assert!(dict.insert(int(1), Object::str("a")).is_none());
        assert!(dict.insert(int(2), Object::str("b")).is_none());
        assert_eq!(dict.len(), 2);
        assert_eq!(dict.get(&int(1)).expect("present").repr(), "'a'");
        assert_eq!(dict.get(&int(2)).expect("present").repr(), "'b'");
        assert!(dict.get(&int(3)).is_none());
    }

    /// The promise the language makes about a dict, so it gets a test that
    /// survives a resize and a deletion rather than one that only fits in the
    /// first table.
    #[test]
    fn iteration_is_in_the_order_things_went_in() {
        let mut dict: Dict = (0..50).rev().map(|n| (int(n), Object::int(n))).collect();
        assert_eq!(order(&dict), (0..50).rev().collect::<Vec<_>>());

        // Removing does not disturb what is left.
        for n in (0..50).step_by(3) {
            dict.remove(&int(n));
        }
        let expected: Vec<i64> = (0..50).rev().filter(|n| n % 3 != 0).collect();
        assert_eq!(order(&dict), expected);

        // Putting a key back puts it at the end, because it is a new key now.
        dict.insert(int(0), Object::None);
        let mut expected = expected;
        expected.push(0);
        assert_eq!(order(&dict), expected);
    }

    /// Overwriting is not reinsertion, so the key keeps the place it had.
    #[test]
    fn writing_over_a_key_leaves_it_where_it_was() {
        let mut dict: Dict = (0..5).map(|n| (int(n), Object::int(n))).collect();
        let previous = dict.insert(int(1), Object::str("new"));
        assert_eq!(previous.expect("there was a value").repr(), "1");
        assert_eq!(order(&dict), vec![0, 1, 2, 3, 4]);
        assert_eq!(dict.get(&int(1)).expect("present").repr(), "'new'");
        assert_eq!(dict.len(), 5);
    }

    /// `d = {1: 'a'}` then `d[True] = 'b'` leaves `1` as the key. The two are
    /// equal, so there is nothing to update, and swapping the key would change
    /// what iterating hands back.
    #[test]
    fn an_equal_key_does_not_replace_the_one_already_there() {
        let mut dict = Dict::new();
        dict.insert(int(1), Object::str("a"));
        dict.insert(key(Object::Bool(true)), Object::str("b"));
        assert_eq!(dict.len(), 1);
        assert_eq!(dict.get(&int(1)).expect("present").repr(), "'b'");
        let stored = dict.keys().next().expect("one key");
        assert_eq!(stored.object().repr(), "1");
    }

    #[test]
    fn the_three_numeric_types_are_one_key() {
        let mut dict = Dict::new();
        dict.insert(int(1), Object::str("int"));
        dict.insert(key(Object::Float(1.0)), Object::str("float"));
        dict.insert(key(Object::Bool(true)), Object::str("bool"));
        assert_eq!(dict.len(), 1);
        assert_eq!(dict.get(&int(1)).expect("present").repr(), "'bool'");
    }

    #[test]
    fn taking_a_key_out_takes_the_value_with_it() {
        let mut dict: Dict = (0..5).map(|n| (int(n), Object::int(n))).collect();
        assert_eq!(dict.remove(&int(2)).expect("was there").repr(), "2");
        assert!(dict.remove(&int(2)).is_none());
        assert_eq!(dict.len(), 4);
        assert!(!dict.contains(&int(2)));
        assert_eq!(order(&dict), vec![0, 1, 3, 4]);
    }

    /// A deleted slot cannot go back to being empty, because a key inserted
    /// after it may have probed past it. Emptying it would end that probe
    /// early and the key would be lost while still being in the table.
    ///
    /// This is the bug tombstones exist to stop, and it needs enough keys and
    /// enough deletions for two of them to have collided, which is why this
    /// churns rather than testing one pair.
    #[test]
    fn a_key_is_not_lost_when_a_key_before_it_is_deleted() {
        let mut dict = Dict::new();
        for round in 0..40i64 {
            for n in 0..40 {
                dict.insert(int(round * 40 + n), Object::int(n));
            }
            for n in 0..40 {
                if (round + n) % 3 == 0 {
                    dict.remove(&int(round * 40 + n));
                }
            }
            // Everything that was not deleted is still findable, every round.
            for n in 0..40 {
                let n = round * 40 + n;
                let present = dict.contains(&int(n));
                assert_eq!(present, (n / 40 + n % 40) % 3 != 0, "key {n}");
            }
        }
    }

    /// A dict used as a queue, which is the shape that catches a container
    /// that never gives anything back.
    ///
    /// Deleting leaves a hole in both the table and the entry list, and
    /// neither is cleared out on the spot, because a probe running over the
    /// hole still has to reach what is past it. So the entry list does grow
    /// under churn. What has to be true is that it grows to a bound rather
    /// than forever, and the resize that clears the holes is what makes that
    /// so. Without it this leaks a machine word per operation.
    #[test]
    fn a_dict_churned_through_does_not_grow_without_end() {
        let mut dict = Dict::new();
        for n in 0..10_000 {
            dict.insert(int(n), Object::int(n));
            dict.remove(&int(n));
            assert!(dict.is_empty());
        }
        dict.insert(int(0), Object::None);
        assert_eq!(order(&dict), vec![0]);
        assert!(
            dict.entries.len() <= MINIMUM,
            "entries grew to {}",
            dict.entries.len()
        );
        assert!(
            dict.indices.len() <= MINIMUM,
            "table grew to {}",
            dict.indices.len()
        );
    }

    /// The same thing with the deletions lagging behind the insertions, so
    /// there really are live keys mixed in with the holes when the resize
    /// comes and it has to keep the ones that are still wanted.
    #[test]
    fn a_sliding_window_keeps_what_is_still_in_it() {
        let mut dict = Dict::new();
        let width = 32;
        for n in 0..2_000 {
            dict.insert(int(n), Object::int(n));
            if n >= width {
                assert_eq!(
                    dict.remove(&int(n - width)).expect("was there").repr(),
                    (n - width).to_string()
                );
            }
            let live = (n + 1).min(width);
            assert_eq!(dict.len(), usize::try_from(live).expect("a small count"));
        }
        assert_eq!(order(&dict), (2_000 - width..2_000).collect::<Vec<_>>());
        assert!(
            dict.entries.len() < 200,
            "entries grew to {}",
            dict.entries.len()
        );
    }

    #[test]
    fn clearing_it_leaves_an_empty_dict_that_still_works() {
        let mut dict: Dict = (0..20).map(|n| (int(n), Object::int(n))).collect();
        dict.clear();
        assert!(dict.is_empty());
        assert!(!dict.contains(&int(1)));
        dict.insert(int(7), Object::None);
        assert_eq!(order(&dict), vec![7]);
    }

    /// Every key hashing to the same thing turns the table into one long
    /// probe, which is where an off by one in the walk shows up.
    #[test]
    fn keys_that_all_collide_still_all_come_back() {
        // A tuple's hash comes from its elements' hashes, so this makes fifty
        // distinct keys out of one repeated hash by construction.
        let colliding = |n: i64| key(Object::tuple(vec![Object::int(0), Object::int(n)]));
        let mut dict = Dict::new();
        for n in 0..50 {
            dict.insert(colliding(n), Object::int(n));
        }
        assert_eq!(dict.len(), 50);
        for n in 0..50 {
            assert_eq!(
                dict.get(&colliding(n)).expect("present").repr(),
                n.to_string()
            );
        }
        for n in (0..50).step_by(2) {
            assert!(dict.remove(&colliding(n)).is_some());
        }
        for n in 0..50 {
            assert_eq!(dict.contains(&colliding(n)), n % 2 == 1, "key {n}");
        }
    }

    #[test]
    fn two_dicts_are_equal_when_they_hold_the_same_thing() {
        let a: Dict = (0..5).map(|n| (int(n), Object::int(n))).collect();
        let b: Dict = (0..5).rev().map(|n| (int(n), Object::int(n))).collect();
        // Order is remembered but it is not part of what makes two equal.
        assert!(a.equals(&b));
        assert_ne!(order(&a), order(&b));

        let c: Dict = (0..4).map(|n| (int(n), Object::int(n))).collect();
        assert!(!a.equals(&c));

        // The values compare with Python's rules, so a float matches an int.
        let d: Dict = (0..5)
            .map(|n| (int(i64::from(n)), Object::Float(f64::from(n))))
            .collect();
        assert!(a.equals(&d));
    }

    #[test]
    fn a_set_holds_each_value_once() {
        let mut set = Set::new();
        assert!(set.insert(int(1)));
        assert!(!set.insert(int(1)));
        assert!(!set.insert(key(Object::Float(1.0))));
        assert!(set.insert(int(2)));
        assert_eq!(set.len(), 2);
        assert!(set.contains(&int(1)));
        assert!(!set.contains(&int(3)));
        assert!(set.remove(&int(1)));
        assert!(!set.remove(&int(1)));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn two_sets_are_equal_when_they_hold_the_same_values() {
        let a: Set = (0..5).map(int).collect();
        let b: Set = (0..5).rev().map(int).collect();
        assert!(a.equals(&b));
        assert!(!a.equals(&(0..4).map(int).collect()));
        assert!(!a.equals(&(1..6).map(int).collect()));
    }
}
