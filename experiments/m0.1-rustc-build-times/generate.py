#!/usr/bin/env python3
"""Emit Rust in the shape `docs/spec/06-aot.md` describes, at a chosen volume.

M0.1 asks how long `rustc` takes on the output of the AOT backend. The backend
does not exist, so this stands in for it. The output is not meant to be good
Rust or to compute anything interesting. It is meant to be wrong in the same
directions the real emitter will be wrong: many small functions, a `Result`
return on every one of them, a shape guard and a cold deopt call in the sealed
ones, the full protocol in the unsealed ones, and calls that cross module
boundaries so the linker and the inliner both have work to do.

The number that matters is Python lines, since that is what the gate in
`docs/spec/10-milestones.md` is stated in, so the expansion model is written
down here rather than left implicit:

    one Python module        250 lines of Python
    one Python function       11 lines of Python, so 22 functions per module
    one Python function        1 Rust function

Those come from measuring the CPython standard library: `statistics.py`,
`json/decoder.py` and `dataclasses.py` average between 9 and 13 lines per
function including decorators, docstrings and blank lines. A 10,000 line
program is therefore about 40 modules and 880 functions, and that is the point
the gate is read at.

    ./generate.py --python-lines 10000 --out generated/
"""

from __future__ import annotations

import argparse
import random
import shutil
from dataclasses import dataclass
from pathlib import Path

LINES_PER_MODULE = 250
LINES_PER_FUNCTION = 11
FUNCTIONS_PER_MODULE = LINES_PER_MODULE // LINES_PER_FUNCTION


@dataclass(frozen=True)
class Plan:
    python_lines: int
    sealed_fraction: float
    seed: int

    @property
    def modules(self) -> int:
        return max(1, round(self.python_lines / LINES_PER_MODULE))

    @property
    def functions(self) -> int:
        return self.modules * FUNCTIONS_PER_MODULE


def sealed_function(name: str, index: int, rng: random.Random, neighbours: list[str]) -> str:
    """The shape from `06-aot.md`: one shape check, unboxed slots, a deopt path.

    This is what `--sealed` produces when the profile was confident, and it is
    the interesting case for build time because the guard, the cold call and
    the checked arithmetic all survive into LLVM as real basic blocks.
    """
    shape = f"SHAPE_{index % 16}"
    call = ""
    if neighbours and rng.random() < 0.35:
        target = rng.choice(neighbours)
        call = f"""
    if vm.truthy(Value::from_small_int(acc)) {{
        let via = {target}(vm, self_)?;
        acc = acc.wrapping_add(via.as_small_int().unwrap_or(0));
    }}"""
    return f"""
/// Sealed. Profile said both slots have always held small integers.
pub fn {name}(vm: &mut Vm, self_: Value) -> Result<Value, Thrown> {{
    if vm.shape_of(self_) != {shape} {{
        return vm.deopt(DEOPT_{name.upper()}, &[self_]);
    }}
    let x = unsafe {{ vm.slot_i64(self_, 0) }};
    let y = unsafe {{ vm.slot_i64(self_, 1) }};
    let mut acc = 0i64;
    for i in 0..{rng.randint(2, 6)}i64 {{
        match x.checked_mul(x + i).and_then(|xx| {{
            y.checked_mul(y).and_then(|yy| xx.checked_add(yy))
        }}) {{
            Some(r) => acc = acc.wrapping_add(r),
            None => return vm.deopt(DEOPT_{name.upper()}, &[self_]),
        }}
    }}{call}
    Ok(Value::from_small_int(acc))
}}
"""


def unsealed_function(name: str, site: int, rng: random.Random, neighbours: list[str]) -> str:
    """The `--open` shape: full protocol, every operation cached but dynamic."""
    call = ""
    if neighbours and rng.random() < 0.35:
        target = rng.choice(neighbours)
        call = f"""
    let extra = vm.call({target}, self_)?;
    let total = vm.binop_add(total, extra)?;"""
    return f"""
/// Unsealed. Nothing was proved, so every operation goes through the protocol.
pub fn {name}(vm: &mut Vm, self_: Value) -> Result<Value, Thrown> {{
    let x = vm.load_attr(self_, ATTR_X, {site})?;
    let y = vm.load_attr(self_, ATTR_Y, {site + 1})?;
    let xx = vm.binop_mul(x, x)?;
    let yy = vm.binop_mul(y, y)?;
    let total = vm.binop_add(xx, yy)?;{call}
    // Bound to a local rather than nested, because two mutable borrows of the
    // vm in one expression do not compile. A real emitter has the same
    // constraint and solves it the same way, since it is emitting from SSA
    // where every intermediate already has a name.
    let small = vm.compare_lt(total, Value::from_small_int({rng.randint(10, 9999)}))?;
    if vm.truthy(small) {{
        vm.store_attr(self_, ATTR_X, total)?;
        return Ok(total);
    }}
    vm.binop_sub(total, Value::from_small_int(1))
}}
"""


def module_source(module_index: int, plan: Plan, rng: random.Random) -> tuple[str, list[str]]:
    names = [f"m{module_index}_f{i}" for i in range(FUNCTIONS_PER_MODULE)]
    parts = [
        f"//! Generated stand-in for Python module {module_index}.",
        "//! Do not edit. See generate.py.",
        "",
        "use m01_runtime::{Thrown, Value, Vm};",
        "",
        "pub const ATTR_X: u32 = 1;",
        "pub const ATTR_Y: u32 = 2;",
    ]
    for i in range(16):
        parts.append(f"pub const SHAPE_{i}: u32 = {i + 1};")
    parts.append("")

    site = module_index * FUNCTIONS_PER_MODULE * 2
    for i, name in enumerate(names):
        emitted_before = names[:i]
        if rng.random() < plan.sealed_fraction:
            parts.append(f"pub const DEOPT_{name.upper()}: u32 = {site};")
            parts.append(sealed_function(name, site, rng, emitted_before))
        else:
            parts.append(unsealed_function(name, site, rng, emitted_before))
        site += 2

    return "\n".join(parts) + "\n", names


def cargo_toml(name: str, runtime_path: str) -> str:
    return f"""# Generated. Do not edit. See generate.py.
[package]
name = "{name}"
version = "0.0.0"
edition = "2024"
publish = false

# Its own workspace so the measurement never drags in the kohebi workspace.
[workspace]

[dependencies]
m01-runtime = {{ path = "{runtime_path}" }}

# Defaults on purpose. The point of the measurement is what a user gets, and a
# profile tuned to make the number look good would be measuring the tuning.
[profile.release]
lto = false
codegen-units = 16

[profile.release-lto]
inherits = "release"
lto = "thin"

# Cargo turns incremental compilation off in release, so an edit to one module
# rebuilds the whole crate at opt-level 3. That is fine for a shipping build and
# hopeless for an edit-run loop, so measure the version that turns it back on.
[profile.release-incr]
inherits = "release"
incremental = true
"""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--python-lines", type=int, default=10_000)
    parser.add_argument(
        "--sealed-fraction",
        type=float,
        default=0.6,
        help="How much of the program the sealing analysis proved. 0.6 is a guess.",
    )
    parser.add_argument("--seed", type=int, default=0)
    parser.add_argument("--out", type=Path, default=Path("generated"))
    parser.add_argument("--name", default="m01_generated")
    parser.add_argument(
        "--runtime-path",
        default="../runtime",
        help="Path to the runtime stand-in, as written into the generated Cargo.toml.",
    )
    args = parser.parse_args()

    plan = Plan(args.python_lines, args.sealed_fraction, args.seed)
    rng = random.Random(args.seed)

    src = args.out / "src"
    if args.out.exists():
        shutil.rmtree(args.out)
    src.mkdir(parents=True)

    all_names: list[tuple[int, list[str]]] = []
    rust_lines = 0
    for m in range(plan.modules):
        text, names = module_source(m, plan, rng)
        (src / f"m{m}.rs").write_text(text)
        rust_lines += text.count("\n")
        all_names.append((m, names))

    mods = "\n".join(f"pub mod m{m};" for m, _ in all_names)
    table = "\n".join(
        f'    ("m{m}::{n}", m{m}::{n} as PyFn),' for m, names in all_names for n in names
    )
    main_rs = f"""//! Generated. Do not edit. See generate.py.
//!
//! Every function is referenced from a table so that nothing is dead code.
//! Without this the linker deletes most of the program and the build time
//! measured is the build time of a program nobody wrote.

use m01_runtime::{{PyFn, Value, Vm}};

{mods}

pub static FUNCTIONS: &[(&str, PyFn)] = &[
{table}
];

fn main() {{
    let mut vm = Vm::new({plan.functions * 2 + 8});
    let shape = vm.define_shape("Point", &[1, 2]);
    let obj = vm.alloc(shape, vec![Value::from_small_int(3), Value::from_small_int(4)]);

    let mut ok = 0u64;
    for (_, f) in FUNCTIONS {{
        if f(&mut vm, obj).is_ok() {{
            ok += 1;
        }}
    }}
    println!("{{}} of {{}} functions returned, {{}} steps", ok, FUNCTIONS.len(), vm.steps);
}}
"""
    (src / "main.rs").write_text(main_rs)
    rust_lines += main_rs.count("\n")
    (args.out / "Cargo.toml").write_text(cargo_toml(args.name, args.runtime_path))

    print(
        f"{plan.python_lines} python lines -> {plan.modules} modules, "
        f"{plan.functions} functions, {rust_lines} lines of Rust "
        f"({rust_lines / plan.python_lines:.1f}x expansion), "
        f"{plan.sealed_fraction:.0%} sealed"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
