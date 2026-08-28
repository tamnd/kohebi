//! `workloads/nbody.py` at three sealing levels.
//!
//! All three run the same algorithm on the same initial conditions and must
//! produce the same two energies as the Python, to nine decimal places. What
//! differs between them is only how much the compiler was allowed to prove.
//!
//! `open`    the full protocol. Attribute access goes through a monomorphic
//!           inline cache, every intermediate is a `Value`, and every float
//!           intermediate is therefore a heap allocation. This is what T1 or a
//!           T2 without good local type inference produces.
//!
//! `typed`   type feedback, no sealing. Arithmetic is unboxed `f64` because the
//!           profile said so, but the shape guard on every attribute access
//!           stays, because nothing proved the class could not change. This is
//!           what `kohebi run` at T2 and `kohebi build --open` should reach.
//!
//! `sealed`  the class is proved closed. Attributes are struct fields at known
//!           offsets, no guards, no caches. This is `--frozen`, and it is the
//!           best case any compiler could emit for this program.
//!
//! The gap between `open` and `sealed` is the whole AOT story. The gap between
//! `typed` and `sealed` is the sealing factor on its own, which is the number
//! `00-README.md` puts at 1.7x and calls its least-supported claim.

use crate::shape::{Cache, SlotKind, define_shape, load_attr, load_attr_f64, store_attr,
    store_attr_f64};
use crate::value::{Value, alloc_instance, binop_add, binop_mul, binop_sub, binop_div};

const SOLAR_MASS: f64 = 4.0 * std::f64::consts::PI * std::f64::consts::PI;
const DAYS_PER_YEAR: f64 = 365.24;

// Attribute names, interned. A real runtime interns these at parse time.
const X: u32 = 0;
const Y: u32 = 1;
const Z: u32 = 2;
const VX: u32 = 3;
const VY: u32 = 4;
const VZ: u32 = 5;
const MASS: u32 = 6;

/// The initial conditions from the standard n-body benchmark: the sun and the
/// four Jovian planets. Written once, in plain `f64`, and handed to whichever
/// representation the variant wants.
const INITIAL: [[f64; 7]; 5] = [
    [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, SOLAR_MASS],
    [
        4.841_431_442_464_720_9e0,
        -1.160_320_044_027_428_4e0,
        -1.036_220_444_711_231_1e-1,
        1.660_076_642_744_036_9e-3 * DAYS_PER_YEAR,
        7.699_011_184_197_404_3e-3 * DAYS_PER_YEAR,
        -6.904_600_169_720_630_2e-5 * DAYS_PER_YEAR,
        9.547_919_384_243_266_1e-4 * SOLAR_MASS,
    ],
    [
        8.343_366_718_244_58e0,
        4.124_798_564_124_305e0,
        -4.035_234_171_143_213_8e-1,
        -2.767_425_107_268_624e-3 * DAYS_PER_YEAR,
        4.998_528_012_349_172_4e-3 * DAYS_PER_YEAR,
        2.304_172_975_737_639_3e-5 * DAYS_PER_YEAR,
        2.858_859_806_661_308e-4 * SOLAR_MASS,
    ],
    [
        1.289_436_956_213_913e1,
        -1.511_115_140_169_863_1e1,
        -2.233_075_788_926_557_3e-1,
        2.964_601_375_647_616_2e-3 * DAYS_PER_YEAR,
        2.378_471_739_594_809_5e-3 * DAYS_PER_YEAR,
        -2.965_895_685_402_375_6e-5 * DAYS_PER_YEAR,
        4.366_244_043_351_563e-5 * SOLAR_MASS,
    ],
    [
        1.537_969_711_485_091_7e1,
        -2.591_931_460_998_796_4e1,
        1.792_587_729_503_711_8e-1,
        2.680_677_724_903_893_2e-3 * DAYS_PER_YEAR,
        1.628_241_700_382_423e-3 * DAYS_PER_YEAR,
        -9.515_922_545_197_158_7e-5 * DAYS_PER_YEAR,
        5.151_389_020_466_114_5e-5 * SOLAR_MASS,
    ],
];

// ---------------------------------------------------------------------------
// sealed: the class is closed, so a Body is a struct and `a.x` is a field.
// ---------------------------------------------------------------------------

/// Still individually heap allocated and still refcounted, because the Python
/// allocates five distinct objects with distinct identities and a compiler is
/// not allowed to flatten that away. What sealing removes is the shape guard
/// and the boxing, not the object.
struct Body {
    x: f64,
    y: f64,
    z: f64,
    vx: f64,
    vy: f64,
    vz: f64,
    mass: f64,
}

pub fn sealed(steps: usize) -> (f64, f64) {
    use std::cell::UnsafeCell;
    use std::rc::Rc;

    let system: Vec<Rc<UnsafeCell<Body>>> = INITIAL
        .iter()
        .map(|b| {
            Rc::new(UnsafeCell::new(Body {
                x: b[0],
                y: b[1],
                z: b[2],
                vx: b[3],
                vy: b[4],
                vz: b[5],
                mass: b[6],
            }))
        })
        .collect();

    let get = |i: usize| unsafe { &mut *system[i].get() };

    let mut px = 0.0;
    let mut py = 0.0;
    let mut pz = 0.0;
    for i in 0..system.len() {
        let b = get(i);
        px -= b.vx * b.mass;
        py -= b.vy * b.mass;
        pz -= b.vz * b.mass;
    }
    {
        let sun = get(0);
        sun.vx = px / SOLAR_MASS;
        sun.vy = py / SOLAR_MASS;
        sun.vz = pz / SOLAR_MASS;
    }

    let energy = |system: &Vec<Rc<UnsafeCell<Body>>>| -> f64 {
        let n = system.len();
        let mut total = 0.0;
        for i in 0..n {
            let a = unsafe { &*system[i].get() };
            total += 0.5 * a.mass * (a.vx * a.vx + a.vy * a.vy + a.vz * a.vz);
            for j in i + 1..n {
                let b = unsafe { &*system[j].get() };
                let dx = a.x - b.x;
                let dy = a.y - b.y;
                let dz = a.z - b.z;
                total -= (a.mass * b.mass) / (dx * dx + dy * dy + dz * dz).sqrt();
            }
        }
        total
    };

    let before = energy(&system);
    let dt = 0.01;
    let n = system.len();
    for _ in 0..steps {
        for i in 0..n {
            for j in i + 1..n {
                let a = get(i);
                let b = get(j);
                let dx = a.x - b.x;
                let dy = a.y - b.y;
                let dz = a.z - b.z;
                let d2 = dx * dx + dy * dy + dz * dz;
                let mag = dt / (d2 * d2.sqrt());
                let am = a.mass * mag;
                let bm = b.mass * mag;
                a.vx -= dx * bm;
                a.vy -= dy * bm;
                a.vz -= dz * bm;
                b.vx += dx * am;
                b.vy += dy * am;
                b.vz += dz * am;
            }
        }
        for i in 0..n {
            let body = get(i);
            body.x += dt * body.vx;
            body.y += dt * body.vy;
            body.z += dt * body.vz;
        }
    }
    (before, energy(&system))
}

// ---------------------------------------------------------------------------
// typed: unboxed arithmetic, but every attribute access still checks the shape.
// ---------------------------------------------------------------------------

/// One cache per syntactic access site, which is what a real compiler emits.
/// Sharing a cache between sites would understate the cost, because a shared
/// cache thrashes and a per-site one does not.
struct TypedCaches {
    load: [Cache; 7],
    store: [Cache; 6],
}

impl TypedCaches {
    fn new() -> TypedCaches {
        TypedCaches {
            load: std::array::from_fn(|_| Cache::new()),
            store: std::array::from_fn(|_| Cache::new()),
        }
    }
}

fn make_system(shape: u32) -> Vec<Value> {
    INITIAL
        .iter()
        .map(|b| alloc_instance(shape, &b.map(f64::to_bits)))
        .collect()
}

pub fn typed(steps: usize) -> (f64, f64) {
    let shape = define_shape(
        "Body",
        &[
            (X, SlotKind::Float),
            (Y, SlotKind::Float),
            (Z, SlotKind::Float),
            (VX, SlotKind::Float),
            (VY, SlotKind::Float),
            (VZ, SlotKind::Float),
            (MASS, SlotKind::Float),
        ],
    );
    let system = make_system(shape);
    let c = TypedCaches::new();
    let n = system.len();

    let mut px = 0.0;
    let mut py = 0.0;
    let mut pz = 0.0;
    for b in &system {
        px -= load_attr_f64(b, VX, &c.load[0]) * load_attr_f64(b, MASS, &c.load[6]);
        py -= load_attr_f64(b, VY, &c.load[1]) * load_attr_f64(b, MASS, &c.load[6]);
        pz -= load_attr_f64(b, VZ, &c.load[2]) * load_attr_f64(b, MASS, &c.load[6]);
    }
    store_attr_f64(&system[0], VX, &c.store[0], px / SOLAR_MASS);
    store_attr_f64(&system[0], VY, &c.store[1], py / SOLAR_MASS);
    store_attr_f64(&system[0], VZ, &c.store[2], pz / SOLAR_MASS);

    let energy = |system: &[Value], c: &TypedCaches| -> f64 {
        let mut total = 0.0;
        for i in 0..system.len() {
            let a = &system[i];
            let (avx, avy, avz) = (
                load_attr_f64(a, VX, &c.load[0]),
                load_attr_f64(a, VY, &c.load[1]),
                load_attr_f64(a, VZ, &c.load[2]),
            );
            let am = load_attr_f64(a, MASS, &c.load[6]);
            total += 0.5 * am * (avx * avx + avy * avy + avz * avz);
            for b in system.iter().skip(i + 1) {
                let dx = load_attr_f64(a, X, &c.load[3]) - load_attr_f64(b, X, &c.load[3]);
                let dy = load_attr_f64(a, Y, &c.load[4]) - load_attr_f64(b, Y, &c.load[4]);
                let dz = load_attr_f64(a, Z, &c.load[5]) - load_attr_f64(b, Z, &c.load[5]);
                total -= (am * load_attr_f64(b, MASS, &c.load[6]))
                    / (dx * dx + dy * dy + dz * dz).sqrt();
            }
        }
        total
    };

    let before = energy(&system, &c);
    let dt = 0.01;
    for _ in 0..steps {
        for i in 0..n {
            for j in i + 1..n {
                let a = &system[i];
                let b = &system[j];
                let dx = load_attr_f64(a, X, &c.load[3]) - load_attr_f64(b, X, &c.load[3]);
                let dy = load_attr_f64(a, Y, &c.load[4]) - load_attr_f64(b, Y, &c.load[4]);
                let dz = load_attr_f64(a, Z, &c.load[5]) - load_attr_f64(b, Z, &c.load[5]);
                let d2 = dx * dx + dy * dy + dz * dz;
                let mag = dt / (d2 * d2.sqrt());
                let am = load_attr_f64(a, MASS, &c.load[6]) * mag;
                let bm = load_attr_f64(b, MASS, &c.load[6]) * mag;
                store_attr_f64(a, VX, &c.store[0], load_attr_f64(a, VX, &c.load[0]) - dx * bm);
                store_attr_f64(a, VY, &c.store[1], load_attr_f64(a, VY, &c.load[1]) - dy * bm);
                store_attr_f64(a, VZ, &c.store[2], load_attr_f64(a, VZ, &c.load[2]) - dz * bm);
                store_attr_f64(b, VX, &c.store[0], load_attr_f64(b, VX, &c.load[0]) + dx * am);
                store_attr_f64(b, VY, &c.store[1], load_attr_f64(b, VY, &c.load[1]) + dy * am);
                store_attr_f64(b, VZ, &c.store[2], load_attr_f64(b, VZ, &c.load[2]) + dz * am);
            }
        }
        for body in &system {
            let nx = load_attr_f64(body, X, &c.load[3]) + dt * load_attr_f64(body, VX, &c.load[0]);
            let ny = load_attr_f64(body, Y, &c.load[4]) + dt * load_attr_f64(body, VY, &c.load[1]);
            let nz = load_attr_f64(body, Z, &c.load[5]) + dt * load_attr_f64(body, VZ, &c.load[2]);
            store_attr_f64(body, X, &c.store[3], nx);
            store_attr_f64(body, Y, &c.store[4], ny);
            store_attr_f64(body, Z, &c.store[5], nz);
        }
    }
    (before, energy(&system, &c))
}

// ---------------------------------------------------------------------------
// hoisted: guards checked once on entry, then a guard-free region.
// ---------------------------------------------------------------------------

/// The variant that stops the sealing factor from being overstated.
///
/// `typed` re-checks the shape at every access site, which is what a naive
/// emitter does. A competent one proves that nothing between the guard and the
/// end of the loop can change an object's shape, checks once, and emits
/// straight-line code with the slot offsets baked in. That is standard, it is
/// what `--open` should reach, and comparing `sealed` against `typed` rather
/// than against this would credit sealing with a win that ordinary guard
/// hoisting already delivers.
///
/// The data still lives in a refcounted heap instance behind a shape id. What
/// has gone is the per-access check, not the object model.
pub fn hoisted(steps: usize) -> (f64, f64) {
    let shape = define_shape(
        "Body",
        &[
            (X, SlotKind::Float),
            (Y, SlotKind::Float),
            (Z, SlotKind::Float),
            (VX, SlotKind::Float),
            (VY, SlotKind::Float),
            (VZ, SlotKind::Float),
            (MASS, SlotKind::Float),
        ],
    );
    let system = make_system(shape);

    // The guard, paid once for the whole run rather than once per access. A
    // real emitter would also record a deopt point here; the deopt path costs
    // nothing when it does not fire, which is the case being modelled. What
    // survives is a base pointer per object, which is what the emitted code
    // holds in a register.
    //
    // The base pointers are a stack array and not a `Vec` on purpose. With a
    // `Vec`, a store through one of the slot pointers may alias the vector's own
    // heap buffer, so LLVM reloads every base pointer after every write and this
    // variant came out 1.75x slower than `sealed`. That gap was an artifact of
    // where the experiment parked its pointers, not a cost of leaving the class
    // open, and taking it at face value would have credited sealing with a win
    // it does not earn.
    let mut base = [std::ptr::null_mut::<u64>(); 5];
    for (i, v) in system.iter().enumerate() {
        let inst = crate::shape::instance_of(v);
        assert_eq!(inst.shape, shape, "guard failed, would deopt here");
        base[i] = inst.slots().as_mut_ptr();
    }
    let base = base;

    // Offsets are compile-time constants after the guard, exactly as they are
    // in emitted code. Reading them back from the shape at runtime would model
    // a compiler that forgot what it had just proved.
    const IX: usize = X as usize;
    const IY: usize = Y as usize;
    const IZ: usize = Z as usize;
    const IVX: usize = VX as usize;
    const IVY: usize = VY as usize;
    const IVZ: usize = VZ as usize;
    const IM: usize = MASS as usize;

    // A slot the shape calls `Float` is eight bytes of `f64`, so the emitted
    // code loads it into a floating-point register. Going through `u64` and
    // `from_bits` would be the same bytes and, on aarch64, an extra `fmov`
    // between the register files on every access, which is a cost of writing
    // the experiment this way rather than a cost of not sealing.
    let ld = |i: usize, k: usize| -> f64 { unsafe { *(base[i].add(k) as *const f64) } };
    let st = |i: usize, k: usize, x: f64| unsafe { *(base[i].add(k) as *mut f64) = x };
    let n = system.len();

    let mut px = 0.0;
    let mut py = 0.0;
    let mut pz = 0.0;
    for i in 0..n {
        let m = ld(i, IM);
        px -= ld(i, IVX) * m;
        py -= ld(i, IVY) * m;
        pz -= ld(i, IVZ) * m;
    }
    st(0, IVX, px / SOLAR_MASS);
    st(0, IVY, py / SOLAR_MASS);
    st(0, IVZ, pz / SOLAR_MASS);

    let energy = || -> f64 {
        let mut total = 0.0;
        for i in 0..n {
            let m = ld(i, IM);
            let (vx, vy, vz) = (ld(i, IVX), ld(i, IVY), ld(i, IVZ));
            total += 0.5 * m * (vx * vx + vy * vy + vz * vz);
            for j in i + 1..n {
                let dx = ld(i, IX) - ld(j, IX);
                let dy = ld(i, IY) - ld(j, IY);
                let dz = ld(i, IZ) - ld(j, IZ);
                total -= (m * ld(j, IM)) / (dx * dx + dy * dy + dz * dz).sqrt();
            }
        }
        total
    };

    let before = energy();
    let dt = 0.01;
    for _ in 0..steps {
        for i in 0..n {
            for j in i + 1..n {
                let dx = ld(i, IX) - ld(j, IX);
                let dy = ld(i, IY) - ld(j, IY);
                let dz = ld(i, IZ) - ld(j, IZ);
                let d2 = dx * dx + dy * dy + dz * dz;
                let mag = dt / (d2 * d2.sqrt());
                let am = ld(i, IM) * mag;
                let bm = ld(j, IM) * mag;
                st(i, IVX, ld(i, IVX) - dx * bm);
                st(i, IVY, ld(i, IVY) - dy * bm);
                st(i, IVZ, ld(i, IVZ) - dz * bm);
                st(j, IVX, ld(j, IVX) + dx * am);
                st(j, IVY, ld(j, IVY) + dy * am);
                st(j, IVZ, ld(j, IVZ) + dz * am);
            }
        }
        for i in 0..n {
            st(i, IX, ld(i, IX) + dt * ld(i, IVX));
            st(i, IY, ld(i, IY) + dt * ld(i, IVY));
            st(i, IZ, ld(i, IZ) + dt * ld(i, IVZ));
        }
    }
    (before, energy())
}

// ---------------------------------------------------------------------------
// open: the full protocol, every intermediate a Value, every float an
// allocation.
// ---------------------------------------------------------------------------

pub fn open(steps: usize) -> (f64, f64) {
    let shape = define_shape(
        "Body",
        &[
            (X, SlotKind::Float),
            (Y, SlotKind::Float),
            (Z, SlotKind::Float),
            (VX, SlotKind::Float),
            (VY, SlotKind::Float),
            (VZ, SlotKind::Float),
            (MASS, SlotKind::Float),
        ],
    );
    let system = make_system(shape);
    let c = TypedCaches::new();
    let n = system.len();

    let mut p = [Value::from_float(0.0), Value::from_float(0.0), Value::from_float(0.0)];
    for b in &system {
        let mass = load_attr(b, MASS, &c.load[6]);
        for (k, attr) in [VX, VY, VZ].into_iter().enumerate() {
            let v = load_attr(b, attr, &c.load[k]);
            p[k] = binop_sub(&p[k], &binop_mul(&v, &mass));
        }
    }
    let solar = Value::from_float(SOLAR_MASS);
    for (k, attr) in [VX, VY, VZ].into_iter().enumerate() {
        store_attr(&system[0], attr, &c.store[k], binop_div(&p[k], &solar));
    }

    let energy = |system: &[Value], c: &TypedCaches| -> f64 {
        let half = Value::from_float(0.5);
        let mut total = Value::from_float(0.0);
        for i in 0..system.len() {
            let a = &system[i];
            let am = load_attr(a, MASS, &c.load[6]);
            let mut sq = Value::from_float(0.0);
            for (k, attr) in [VX, VY, VZ].into_iter().enumerate() {
                let v = load_attr(a, attr, &c.load[k]);
                sq = binop_add(&sq, &binop_mul(&v, &v));
            }
            total = binop_add(&total, &binop_mul(&binop_mul(&half, &am), &sq));
            for b in system.iter().skip(i + 1) {
                let mut d2 = Value::from_float(0.0);
                for (k, attr) in [X, Y, Z].into_iter().enumerate() {
                    let d = binop_sub(&load_attr(a, attr, &c.load[3 + k]),
                        &load_attr(b, attr, &c.load[3 + k]));
                    d2 = binop_add(&d2, &binop_mul(&d, &d));
                }
                let dist = Value::from_float(d2.as_float().unwrap().sqrt());
                let bm = load_attr(b, MASS, &c.load[6]);
                total = binop_sub(&total, &binop_div(&binop_mul(&am, &bm), &dist));
            }
        }
        total.as_float().unwrap()
    };

    let before = energy(&system, &c);
    let dt = Value::from_float(0.01);
    for _ in 0..steps {
        for i in 0..n {
            for j in i + 1..n {
                let a = &system[i];
                let b = &system[j];
                let mut d = [Value::none(), Value::none(), Value::none()];
                let mut d2 = Value::from_float(0.0);
                for (k, attr) in [X, Y, Z].into_iter().enumerate() {
                    d[k] = binop_sub(&load_attr(a, attr, &c.load[3 + k]),
                        &load_attr(b, attr, &c.load[3 + k]));
                    d2 = binop_add(&d2, &binop_mul(&d[k], &d[k]));
                }
                let d2f = d2.as_float().unwrap();
                let mag = binop_div(&dt, &Value::from_float(d2f * d2f.sqrt()));
                let am = binop_mul(&load_attr(a, MASS, &c.load[6]), &mag);
                let bm = binop_mul(&load_attr(b, MASS, &c.load[6]), &mag);
                for (k, attr) in [VX, VY, VZ].into_iter().enumerate() {
                    let av = binop_sub(&load_attr(a, attr, &c.load[k]), &binop_mul(&d[k], &bm));
                    store_attr(a, attr, &c.store[k], av);
                    let bv = binop_add(&load_attr(b, attr, &c.load[k]), &binop_mul(&d[k], &am));
                    store_attr(b, attr, &c.store[k], bv);
                }
            }
        }
        for body in &system {
            for (k, (pos, vel)) in [(X, VX), (Y, VY), (Z, VZ)].into_iter().enumerate() {
                let moved = binop_add(
                    &load_attr(body, pos, &c.load[3 + k]),
                    &binop_mul(&dt, &load_attr(body, vel, &c.load[k])),
                );
                store_attr(body, pos, &c.store[3 + k], moved);
            }
        }
    }
    (before, energy(&system, &c))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant has to agree with every other and with the Python. These
    /// are the values `workloads/nbody.py` prints, and they are also the
    /// published reference values for the benchmark, so they check the physics
    /// as well as the port.
    #[test]
    fn all_four_variants_agree_with_python() {
        let expected = (-0.169_075_164, -0.169_087_605);
        for (name, got) in [
            ("sealed", sealed(1000)),
            ("hoisted", hoisted(1000)),
            ("typed", typed(1000)),
            ("open", open(1000)),
        ] {
            assert!(
                (got.0 - expected.0).abs() < 1e-9,
                "{name} initial energy {} != {}",
                got.0,
                expected.0
            );
            assert!(
                (got.1 - expected.1).abs() < 1e-9,
                "{name} final energy {} != {}",
                got.1,
                expected.1
            );
        }
    }
}
