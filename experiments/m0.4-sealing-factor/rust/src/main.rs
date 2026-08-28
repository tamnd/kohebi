//! M0.4: hand-written Rust at three sealing levels.
//!
//!     m04 <workload> <variant> [n]
//!
//! Prints exactly what the matching Python in `workloads/` prints, so the two
//! can be diffed rather than eyeballed. An answer that does not match is not a
//! faster implementation of the workload, it is a different workload.

mod interp;
mod name;
mod nbody;
mod shape;
mod value;

#[cfg(feature = "pool")]
mod pool;

/// The allocator is the variable, so say which one is in the binary rather than
/// leaving a reader of the results to guess.
#[cfg(feature = "pool")]
#[global_allocator]
static ALLOC: pool::Pool = pool::Pool;

pub const ALLOCATOR: &str = if cfg!(feature = "pool") {
    "pooled"
} else {
    "system"
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: m04 <nbody> <open|typed|hoisted|sealed> [n]");
        std::process::exit(2);
    }
    let workload = args[1].as_str();
    let variant = args[2].as_str();
    // Same defaults as the matching Python, so `m04 nbody sealed` and
    // `python3 workloads/nbody.py` do the same amount of work.
    let default = match workload {
        "interp" => 1,
        _ => 1_000_000,
    };
    let n: usize = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(default);

    match workload {
        "nbody" => {
            let (before, after) = match variant {
                "open" => nbody::open(n),
                "typed" => nbody::typed(n),
                "hoisted" => nbody::hoisted(n),
                "sealed" => nbody::sealed(n),
                other => unknown(other),
            };
            println!("{before:.9}");
            println!("{after:.9}");
        }
        "interp" => {
            let result = match variant {
                "open" => interp::open(n),
                "sealed" => interp::sealed(n),
                other => unknown(other),
            };
            println!("{result}");
        }
        other => unknown(other),
    }
}

fn unknown(what: &str) -> ! {
    eprintln!("unknown: {what}");
    std::process::exit(2);
}
