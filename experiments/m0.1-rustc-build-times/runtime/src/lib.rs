//! A stand-in for `kohebi-core`, sized to make the build-time measurement honest.
//!
//! The question M0.1 asks is how long `rustc` takes on the Rust our AOT backend
//! will emit. That number depends on what the emitted code calls into, so this
//! crate exists to be that thing: tagged values, shapes, unboxed slot access,
//! the binary operator protocol, and a deopt entry point, with the same
//! inlining decisions the real one would make.
//!
//! It is not `kohebi-core` and it is not going to become it. Nothing here is
//! correct Python, the object graph is a toy, and the collector does not exist.
//! What matters for the measurement is the shape of the interface: which calls
//! cross a crate boundary, which are `#[inline]` and therefore work LLVM does
//! at every call site, and which are cold enough to be `#[inline(never)]`.

#![allow(clippy::missing_safety_doc)]

use std::collections::HashMap;

/// Three tag bits in the low end of a 64 bit word, per `docs/spec/03-object-model.md`.
pub const TAG_BITS: u64 = 3;
pub const TAG_MASK: u64 = 0b111;

pub const TAG_OBJECT: u64 = 0b000;
pub const TAG_SMALL_INT: u64 = 0b001;
pub const TAG_BOOL: u64 = 0b010;
pub const TAG_NONE: u64 = 0b011;
pub const TAG_FLOAT: u64 = 0b100;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(transparent)]
pub struct Value(pub u64);

impl Value {
    pub const NONE: Value = Value(TAG_NONE);

    #[inline]
    pub fn from_small_int(n: i64) -> Value {
        Value(((n as u64) << TAG_BITS) | TAG_SMALL_INT)
    }

    #[inline]
    pub fn as_small_int(self) -> Option<i64> {
        if self.0 & TAG_MASK == TAG_SMALL_INT {
            Some((self.0 as i64) >> TAG_BITS)
        } else {
            None
        }
    }

    #[inline]
    pub fn from_bool(b: bool) -> Value {
        Value(((b as u64) << TAG_BITS) | TAG_BOOL)
    }

    #[inline]
    pub fn is_object(self) -> bool {
        self.0 & TAG_MASK == TAG_OBJECT && self.0 != 0
    }

    /// The object index this value points at.
    ///
    /// Real kohebi stores a pointer here. The experiment stores an index into a
    /// side table, which keeps the crate safe to run while emitting the same
    /// shift-and-mask work at the call site.
    #[inline]
    pub fn as_object(self) -> ObjectRef {
        ObjectRef((self.0 >> TAG_BITS) as u32)
    }

    #[inline]
    pub fn from_object(o: ObjectRef) -> Value {
        Value((o.0 as u64) << TAG_BITS)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ObjectRef(pub u32);

pub type ShapeId = u32;

pub const SHAPE_INVALID: ShapeId = 0;

/// What a failed operation returns. Cheap to move, since it is on the cold path
/// of every emitted function and its size lands in every `Result` return.
#[derive(Clone, Debug)]
pub struct Thrown {
    pub kind: &'static str,
    pub message: String,
}

impl Thrown {
    #[inline(never)]
    #[cold]
    pub fn type_error(message: impl Into<String>) -> Thrown {
        Thrown { kind: "TypeError", message: message.into() }
    }

    #[inline(never)]
    #[cold]
    pub fn attribute_error(name: &str) -> Thrown {
        Thrown { kind: "AttributeError", message: format!("no attribute {name}") }
    }
}

/// A heap object: a shape and a run of slots.
///
/// The 16 byte header from the spec is not modelled. Layout affects the memory
/// numbers in M2 and does not affect how long `rustc` takes, which is the only
/// thing being measured here.
pub struct Object {
    pub shape: ShapeId,
    pub slots: Vec<Value>,
}

#[derive(Default)]
pub struct Shape {
    pub name: String,
    pub attrs: HashMap<u32, usize>,
}

/// One inline cache site. The emitted code indexes these by a constant, which
/// is what makes the cache lookup a load rather than a hash.
#[derive(Clone, Copy, Default)]
pub struct InlineCache {
    pub shape: ShapeId,
    pub slot: u32,
    pub hits: u32,
    pub misses: u32,
}

#[derive(Default)]
pub struct DeoptStats {
    pub taken: u64,
    pub by_site: HashMap<u32, u64>,
}

#[derive(Default)]
pub struct Vm {
    pub objects: Vec<Object>,
    pub shapes: Vec<Shape>,
    pub caches: Vec<InlineCache>,
    pub deopts: DeoptStats,
    pub steps: u64,
}

impl Vm {
    pub fn new(cache_sites: usize) -> Vm {
        Vm {
            caches: vec![InlineCache::default(); cache_sites],
            // Index 0 is burned on purpose. An object at index 0 encodes to the
            // all-zero Value, which is the null pattern, so the first object
            // allocated would read back as "not an object" and every attribute
            // load on it would take the error path.
            objects: vec![Object { shape: SHAPE_INVALID, slots: Vec::new() }],
            ..Vm::default()
        }
    }

    pub fn define_shape(&mut self, name: &str, attrs: &[u32]) -> ShapeId {
        let mut shape = Shape { name: name.to_string(), attrs: HashMap::new() };
        for (i, a) in attrs.iter().enumerate() {
            shape.attrs.insert(*a, i);
        }
        self.shapes.push(shape);
        self.shapes.len() as ShapeId
    }

    pub fn alloc(&mut self, shape: ShapeId, slots: Vec<Value>) -> Value {
        self.objects.push(Object { shape, slots });
        Value::from_object(ObjectRef(self.objects.len() as u32 - 1))
    }

    #[inline]
    pub fn shape_of(&self, v: Value) -> ShapeId {
        if !v.is_object() {
            return SHAPE_INVALID;
        }
        self.objects[v.as_object().0 as usize].shape
    }

    /// Unsealed attribute load: cache probe, then the full lookup.
    ///
    /// `#[inline]` on purpose. This is the single most common operation in
    /// unsealed output, so whether it inlines is most of the difference between
    /// a fast build and a fast binary, which is exactly the trade M0.1 is about.
    #[inline]
    pub fn load_attr(&mut self, obj: Value, name: u32, site: u32) -> Result<Value, Thrown> {
        self.steps += 1;
        let shape = self.shape_of(obj);
        let cache = self.caches[site as usize];
        if cache.shape == shape && shape != SHAPE_INVALID {
            self.caches[site as usize].hits += 1;
            return Ok(self.objects[obj.as_object().0 as usize].slots[cache.slot as usize]);
        }
        self.load_attr_slow(obj, name, site)
    }

    #[inline(never)]
    pub fn load_attr_slow(&mut self, obj: Value, name: u32, site: u32) -> Result<Value, Thrown> {
        let shape = self.shape_of(obj);
        if shape == SHAPE_INVALID {
            return Err(Thrown::attribute_error("<attr>"));
        }
        let slot = match self.shapes[shape as usize - 1].attrs.get(&name) {
            Some(s) => *s,
            None => return Err(Thrown::attribute_error("<attr>")),
        };
        self.caches[site as usize] =
            InlineCache { shape, slot: slot as u32, hits: 0, misses: cache_misses(self, site) };
        Ok(self.objects[obj.as_object().0 as usize].slots[slot])
    }

    #[inline]
    pub fn store_attr(&mut self, obj: Value, name: u32, value: Value) -> Result<(), Thrown> {
        self.steps += 1;
        let shape = self.shape_of(obj);
        if shape == SHAPE_INVALID {
            return Err(Thrown::attribute_error("<attr>"));
        }
        let slot = match self.shapes[shape as usize - 1].attrs.get(&name) {
            Some(s) => *s,
            None => return Err(Thrown::attribute_error("<attr>")),
        };
        self.objects[obj.as_object().0 as usize].slots[slot] = value;
        Ok(())
    }

    #[inline]
    pub fn binop_add(&mut self, a: Value, b: Value) -> Result<Value, Thrown> {
        self.steps += 1;
        match (a.as_small_int(), b.as_small_int()) {
            (Some(x), Some(y)) => match x.checked_add(y) {
                Some(r) => Ok(Value::from_small_int(r)),
                None => self.overflow_add(x, y),
            },
            _ => Err(Thrown::type_error("unsupported operand type(s) for +")),
        }
    }

    #[inline]
    pub fn binop_mul(&mut self, a: Value, b: Value) -> Result<Value, Thrown> {
        self.steps += 1;
        match (a.as_small_int(), b.as_small_int()) {
            (Some(x), Some(y)) => match x.checked_mul(y) {
                Some(r) => Ok(Value::from_small_int(r)),
                None => self.overflow_mul(x, y),
            },
            _ => Err(Thrown::type_error("unsupported operand type(s) for *")),
        }
    }

    #[inline]
    pub fn binop_sub(&mut self, a: Value, b: Value) -> Result<Value, Thrown> {
        self.steps += 1;
        match (a.as_small_int(), b.as_small_int()) {
            (Some(x), Some(y)) => Ok(Value::from_small_int(x.wrapping_sub(y))),
            _ => Err(Thrown::type_error("unsupported operand type(s) for -")),
        }
    }

    #[inline]
    pub fn compare_lt(&mut self, a: Value, b: Value) -> Result<Value, Thrown> {
        match (a.as_small_int(), b.as_small_int()) {
            (Some(x), Some(y)) => Ok(Value::from_bool(x < y)),
            _ => Err(Thrown::type_error("unorderable")),
        }
    }

    #[inline]
    pub fn truthy(&mut self, v: Value) -> bool {
        match v.0 & TAG_MASK {
            TAG_NONE => false,
            TAG_BOOL => v.0 >> TAG_BITS != 0,
            TAG_SMALL_INT => v.as_small_int() != Some(0),
            _ => true,
        }
    }

    /// Sealed code reads slots without a protocol call once the shape check passes.
    #[inline]
    pub unsafe fn slot_i64(&self, obj: Value, slot: usize) -> i64 {
        let o = &self.objects[obj.as_object().0 as usize];
        o.slots[slot].as_small_int().unwrap_or(0)
    }

    /// The path a failed guard in sealed code takes back to the interpreter.
    #[inline(never)]
    #[cold]
    pub fn deopt(&mut self, site: u32, _live: &[Value]) -> Result<Value, Thrown> {
        self.deopts.taken += 1;
        *self.deopts.by_site.entry(site).or_insert(0) += 1;
        Ok(Value::NONE)
    }

    #[inline(never)]
    #[cold]
    fn overflow_add(&mut self, x: i64, y: i64) -> Result<Value, Thrown> {
        Ok(Value::from_small_int((x as i128 + y as i128) as i64))
    }

    #[inline(never)]
    #[cold]
    fn overflow_mul(&mut self, x: i64, y: i64) -> Result<Value, Thrown> {
        Ok(Value::from_small_int((x as i128 * y as i128) as i64))
    }

    #[inline(never)]
    pub fn call(&mut self, f: PyFn, arg: Value) -> Result<Value, Thrown> {
        f(self, arg)
    }
}

fn cache_misses(vm: &Vm, site: u32) -> u32 {
    vm.caches[site as usize].misses.saturating_add(1)
}

/// The signature every emitted function has.
pub type PyFn = fn(&mut Vm, Value) -> Result<Value, Thrown>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_ints_round_trip() {
        for n in [-1000i64, -1, 0, 1, 1000, i32::MAX as i64] {
            assert_eq!(Value::from_small_int(n).as_small_int(), Some(n));
        }
    }

    #[test]
    fn attribute_load_uses_the_cache_on_the_second_hit() {
        let mut vm = Vm::new(4);
        let shape = vm.define_shape("Point", &[1, 2]);
        let p = vm.alloc(shape, vec![Value::from_small_int(3), Value::from_small_int(4)]);
        assert_eq!(vm.load_attr(p, 1, 0).unwrap().as_small_int(), Some(3));
        assert_eq!(vm.load_attr(p, 1, 0).unwrap().as_small_int(), Some(3));
        assert_eq!(vm.caches[0].hits, 1);
    }

    #[test]
    fn deopt_is_counted_per_site() {
        let mut vm = Vm::new(1);
        vm.deopt(7, &[]).unwrap();
        vm.deopt(7, &[]).unwrap();
        assert_eq!(vm.deopts.by_site[&7], 2);
    }
}
