//! Shapes, typed unboxed slots, and monomorphic inline caches.
//!
//! This is the part of `03-object-model.md` the guarded variants have to pay
//! for and the sealed variants get to skip:
//!
//! > Each slot in the shape records what has actually been stored in it: `i64`,
//! > `f64`, `bool`, or boxed pointer. If a slot has only ever held integers, it
//! > holds a raw `i64` with no allocation, no tag, and no indirection.
//!
//! So an instance is a shape id plus a run of raw 64-bit words, and the shape
//! is what says whether a given word is an `f64`, an `i64`, or a tagged
//! pointer. Reading an `f64` slot costs a shape check and a load. It does not
//! cost an allocation, which matters for the honesty of the comparison: the
//! guarded variants are slower than the sealed ones because of the guards and
//! because of what happens to the value afterwards, not because the object
//! model was written badly on purpose.
//!
//! The shape table is a process global rather than a parameter, which is what
//! kohebi has and what lets an instance release its pointer slots when it dies
//! without carrying a copy of its own layout. Nothing on the cache hit path
//! touches it.

use std::cell::{Cell, RefCell};

use crate::value::{Instance, Obj, Value};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotKind {
    Int,
    Float,
    Ref,
}

pub struct Shape {
    pub name: &'static str,
    pub attrs: Vec<(u32, SlotKind)>,
}

impl Shape {
    fn lookup(&self, attr: u32) -> Option<(u32, SlotKind)> {
        self.attrs
            .iter()
            .enumerate()
            .find(|(_, (a, _))| *a == attr)
            .map(|(i, (_, k))| (i as u32, *k))
    }
}

thread_local! {
    static SHAPES: RefCell<Vec<Shape>> = const { RefCell::new(Vec::new()) };
}

pub fn define_shape(name: &'static str, attrs: &[(u32, SlotKind)]) -> u32 {
    SHAPES.with_borrow_mut(|shapes| {
        shapes.push(Shape {
            name,
            attrs: attrs.to_vec(),
        });
        (shapes.len() - 1) as u32
    })
}

fn lookup(shape: u32, attr: u32) -> (u32, SlotKind) {
    SHAPES.with_borrow(|shapes| {
        let s = &shapes[shape as usize];
        s.lookup(attr)
            .unwrap_or_else(|| panic!("no attribute {attr} on shape {}", s.name))
    })
}

/// Which slots of a shape hold references, so a dying instance can release
/// them. Returns nothing if the shape table is already gone, which only
/// happens during thread teardown when the process is exiting anyway.
fn ref_slots(shape: u32) -> Vec<usize> {
    SHAPES
        .try_with(|cell| {
            cell.borrow()[shape as usize]
                .attrs
                .iter()
                .enumerate()
                .filter(|(_, (_, k))| *k == SlotKind::Ref)
                .map(|(i, _)| i)
                .collect()
        })
        .unwrap_or_default()
}

impl Drop for Instance {
    fn drop(&mut self) {
        // Only pointer slots own anything. An f64 or i64 slot is a raw word and
        // there is nothing to release.
        let refs = ref_slots(self.shape);
        if refs.is_empty() {
            return;
        }
        let slots = self.slots.get_mut();
        for i in refs {
            drop(Value::from_raw(slots[i]));
        }
    }
}

/// A monomorphic inline cache. One shape, one slot index, one kind. A miss
/// costs a search of the shape and a rewrite; a hit costs the comparison that
/// sealing is able to delete.
pub struct Cache {
    shape: Cell<u32>,
    index: Cell<u32>,
    kind: Cell<SlotKind>,
    pub misses: Cell<u32>,
}

impl Default for Cache {
    fn default() -> Cache {
        Cache::new()
    }
}

impl Cache {
    pub const fn new() -> Cache {
        Cache {
            // No shape has this id, so the first look is always a miss.
            shape: Cell::new(u32::MAX),
            index: Cell::new(0),
            kind: Cell::new(SlotKind::Ref),
            misses: Cell::new(0),
        }
    }

    #[inline(always)]
    fn resolve(&self, inst: &Instance, attr: u32) {
        if inst.shape != self.shape.get() {
            self.fill(inst.shape, attr);
        }
    }

    #[inline(never)]
    #[cold]
    fn fill(&self, shape: u32, attr: u32) {
        let (index, kind) = lookup(shape, attr);
        self.shape.set(shape);
        self.index.set(index);
        self.kind.set(kind);
        self.misses.set(self.misses.get() + 1);
    }
}

#[inline(always)]
pub fn instance_of(v: &Value) -> &Instance {
    match v.as_obj() {
        Obj::Instance(i) => i,
        _ => panic!("attribute access on a non-instance"),
    }
}

/// The guarded attribute load. Check the cached shape, then read the slot the
/// shape describes. An `f64` slot allocates on the way out, because the result
/// has to become a `Value` and floats are not immediates.
#[inline(always)]
pub fn load_attr(obj: &Value, attr: u32, cache: &Cache) -> Value {
    let inst = instance_of(obj);
    cache.resolve(inst, attr);
    let word = inst.slots()[cache.index.get() as usize];
    match cache.kind.get() {
        SlotKind::Int => Value::from_int(word as i64),
        SlotKind::Float => Value::from_float(f64::from_bits(word)),
        SlotKind::Ref => Value::clone_from_raw(word),
    }
}

/// The same load without the boxing, for the variant that has type feedback but
/// still needs the guard. The gap between this and the sealed version is the
/// sealing factor proper, with the cost of boxing held constant.
#[inline(always)]
pub fn load_attr_f64(obj: &Value, attr: u32, cache: &Cache) -> f64 {
    let inst = instance_of(obj);
    cache.resolve(inst, attr);
    debug_assert_eq!(cache.kind.get(), SlotKind::Float);
    f64::from_bits(inst.slots()[cache.index.get() as usize])
}

#[inline(always)]
pub fn store_attr_f64(obj: &Value, attr: u32, cache: &Cache, x: f64) {
    let inst = instance_of(obj);
    cache.resolve(inst, attr);
    debug_assert_eq!(cache.kind.get(), SlotKind::Float);
    inst.slots()[cache.index.get() as usize] = x.to_bits();
}

#[inline(always)]
pub fn store_attr(obj: &Value, attr: u32, cache: &Cache, v: Value) {
    let inst = instance_of(obj);
    cache.resolve(inst, attr);
    let index = cache.index.get() as usize;
    let slots = inst.slots();
    match cache.kind.get() {
        SlotKind::Float => slots[index] = v.as_float().expect("float slot").to_bits(),
        SlotKind::Int => slots[index] = v.as_int() as u64,
        SlotKind::Ref => {
            // The old occupant loses a reference and the new one gains it. This
            // is the write barrier a refcounted runtime cannot skip, and
            // sealing does not remove it either.
            let old = Value::from_raw(slots[index]);
            slots[index] = v.into_raw();
            drop(old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::alloc_instance;

    const X: u32 = 0;
    const Y: u32 = 1;

    fn point() -> u32 {
        define_shape("Point", &[(X, SlotKind::Float), (Y, SlotKind::Float)])
    }

    #[test]
    fn a_float_slot_reads_back_exactly() {
        let p = alloc_instance(point(), &[1.5f64.to_bits(), 2.5f64.to_bits()]);
        let c = Cache::new();
        assert_eq!(load_attr_f64(&p, Y, &c), 2.5);
    }

    #[test]
    fn the_cache_misses_once_and_then_hits() {
        let p = alloc_instance(point(), &[1.5f64.to_bits(), 2.5f64.to_bits()]);
        let c = Cache::new();
        for _ in 0..10 {
            load_attr_f64(&p, X, &c);
        }
        assert_eq!(c.misses.get(), 1);
    }

    #[test]
    fn storing_through_the_protocol_is_visible_to_a_later_load() {
        let p = alloc_instance(point(), &[0f64.to_bits(), 0f64.to_bits()]);
        let store = Cache::new();
        let load = Cache::new();
        store_attr_f64(&p, X, &store, 9.25);
        assert_eq!(load_attr_f64(&p, X, &load), 9.25);
    }

    #[test]
    fn a_pointer_slot_releases_its_occupant_when_the_instance_dies() {
        let boxed = define_shape("Boxed", &[(X, SlotKind::Ref)]);
        let inner = Value::from_float(3.5);
        let held = inner.clone();
        let outer = alloc_instance(boxed, &[inner.into_raw()]);
        drop(outer);
        // If the instance had leaked its slot this would still read 3.5 and the
        // test would pass for the wrong reason, so check the count instead.
        assert_eq!(held.strong_count(), 1);
    }
}
