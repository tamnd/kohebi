//! M0.3: how fast is Cranelift on the shape of IR a Python tier 2 produces,
//! and how good is the code it gives back.
//!
//!     m03 cranelift --ops 256 --objects 8 --iters 20000 --opt none
//!     m03 emit-ir   --ops 256 --objects 8            > trace.ll
//!     m03 emit-driver --ops 256 --objects 8 --iters 20000 > driver.c
//!
//! `measure.py` drives all three. See README.md for what the numbers mean.

mod clif;
mod llvm;
mod trace;

use std::process::ExitCode;

use trace::Trace;

struct Args {
    ops: usize,
    objects: i64,
    iters: i64,
    opt: clif::OptLevel,
    deopt_at: Option<i64>,
    compiles: usize,
    timing: bool,
    state: clif::DeoptState,
    vcode: bool,
}

fn median(mut xs: Vec<u128>) -> u128 {
    xs.sort_unstable();
    xs[xs.len() / 2]
}

fn parse(mut it: impl Iterator<Item = String>) -> Result<(String, Args), String> {
    let cmd = it.next().ok_or("missing subcommand")?;
    let mut a = Args {
        ops: 256,
        objects: 8,
        iters: 20_000,
        opt: clif::OptLevel::None,
        deopt_at: None,
        compiles: 5,
        timing: false,
        state: clif::DeoptState::Ssa,
        vcode: false,
    };
    while let Some(flag) = it.next() {
        // The only flag that does not take a value, handled before the fetch
        // below so that `--timing` at the end of the line is not read as a flag
        // missing its argument.
        if flag == "--timing" {
            a.timing = true;
            continue;
        }
        if flag == "--vcode" {
            a.vcode = true;
            continue;
        }
        let value = it.next().ok_or(format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--ops" => a.ops = value.parse().map_err(|_| "--ops wants a number")?,
            "--objects" => a.objects = value.parse().map_err(|_| "--objects wants a number")?,
            "--iters" => a.iters = value.parse().map_err(|_| "--iters wants a number")?,
            "--opt" => {
                a.opt = clif::OptLevel::parse(&value).ok_or("--opt is none or speed")?;
            }
            "--deopt-state" => {
                a.state = clif::DeoptState::parse(&value)
                    .ok_or("--deopt-state is ssa or spilled")?;
            }
            "--deopt-at" => {
                a.deopt_at = Some(value.parse().map_err(|_| "--deopt-at wants a number")?);
            }
            "--compiles" => {
                a.compiles = value.parse().map_err(|_| "--compiles wants a number")?;
                if a.compiles == 0 {
                    return Err("--compiles wants at least one".into());
                }
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok((cmd, a))
}

fn main() -> ExitCode {
    let (cmd, args) = match parse(std::env::args().skip(1)) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("m03: {e}");
            return ExitCode::FAILURE;
        }
    };
    let t = Trace::new(args.ops, args.objects);

    match cmd.as_str() {
        "cranelift" => {
            // Compile the same trace several times in one process. The first
            // compile pays for building the ISA tables and touching pages the
            // process has never touched, which a long lived runtime pays once
            // and not per function, so it is reported on its own rather than
            // averaged into a number that describes neither case.
            let mut runs = Vec::with_capacity(args.compiles);
            for _ in 0..args.compiles {
                let mut build = clif::Build::new(args.opt, args.state);
                build.break_shape = args.deopt_at;
                build.want_vcode = args.vcode;
                match clif::compile_and_run(&t, args.iters, build) {
                    Ok(c) => runs.push(c),
                    Err(e) => {
                        eprintln!("m03: cranelift: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            if args.timing {
                // Where the compile time went, pass by pass, summed over every
                // compile in this process. Knowing that Cranelift is slow on a
                // trace this shape is a symptom; knowing which pass is slow is
                // something M6 can act on. Goes to stderr so the JSON on stdout
                // stays parseable.
                eprint!("{}", cranelift_codegen::timing::take_current());
            }
            if let Some(text) = runs[0].vcode.as_deref() {
                eprint!("{text}");
            }
            let expected = trace::evaluate(&t, args.iters);
            // A back end that gets the wrong answer quickly has not won
            // anything, so the answer is part of the record, not a precondition
            // checked once and forgotten.
            let ok = runs.iter().all(|c| {
                args.deopt_at.is_some() || c.out.to_bits() == expected.to_bits()
            });
            let first = &runs[0];
            println!(
                "{{\"backend\": \"cranelift\", \"opt\": \"{opt}\", \
                 \"deopt_state\": \"{state}\", \"ops\": {ops}, \
                 \"blocks\": {blocks}, \"objects\": {objects}, \"iters\": {iters}, \
                 \"compiles\": {compiles}, \"ir_ns\": {ir}, \
                 \"compile_first_ns\": {first_compile}, \"compile_ns\": {compile}, \
                 \"code_bytes\": {bytes}, \"insts\": {insts}, \
                 \"biggest_block\": {biggest}, \"run_ns\": {run}, \"ret\": {ret}, \
                 \"out_bits\": {out}, \"ok\": {ok}}}",
                opt = match args.opt {
                    clif::OptLevel::None => "none",
                    clif::OptLevel::Speed => "speed",
                },
                state = args.state.name(),
                ops = args.ops,
                blocks = t.blocks(),
                objects = args.objects,
                iters = args.iters,
                compiles = args.compiles,
                ir = median(runs.iter().map(|c| c.ir_ns).collect()),
                first_compile = first.compile_ns,
                compile = median(runs.iter().map(|c| c.compile_ns).collect()),
                bytes = first.code_bytes,
                insts = first.insts,
                biggest = first.biggest_block,
                run = median(runs.iter().map(|c| c.run_ns).collect()),
                ret = first.ret,
                out = first.out.to_bits(),
            );
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        "emit-ir" => {
            print!("{}", llvm::emit_ir(&t));
            ExitCode::SUCCESS
        }
        "emit-driver" => {
            print!("{}", llvm::emit_driver(&t, args.iters));
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("m03: unknown subcommand {other}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small() -> Trace {
        Trace::new(16, 4)
    }

    #[test]
    fn cranelift_computes_what_the_reference_computes() {
        let t = small();
        let c = clif::compile_and_run(&t, 500, clif::Build::new(clif::OptLevel::None, clif::DeoptState::Ssa)).unwrap();
        assert_eq!(c.out.to_bits(), trace::evaluate(&t, 500).to_bits());
        assert_eq!(c.ret, 0);
    }

    #[test]
    fn the_two_optimization_levels_agree_bit_for_bit() {
        // If they did not, one of them is reassociating floating point, and a
        // run-time comparison between them would be comparing two programs.
        let t = small();
        let none = clif::compile_and_run(&t, 500, clif::Build::new(clif::OptLevel::None, clif::DeoptState::Ssa)).unwrap();
        let speed = clif::compile_and_run(&t, 500, clif::Build::new(clif::OptLevel::Speed, clif::DeoptState::Ssa)).unwrap();
        assert_eq!(none.out.to_bits(), speed.out.to_bits());
    }

    #[test]
    fn a_broken_shape_takes_the_cold_path() {
        // The deopt blocks are compiled on every run and entered on none of
        // them, which is exactly the situation where a bug lives a long time.
        let t = small();
        let mut build = clif::Build::new(clif::OptLevel::None, clif::DeoptState::Ssa);
        build.break_shape = Some(1);
        let c = clif::compile_and_run(&t, 500, build).unwrap();
        // Object 1 is first guarded at operation 1, and the stub returns id + 1.
        assert_eq!(c.ret, 2);
    }

    #[test]
    fn code_size_grows_with_the_trace() {
        let a = clif::compile_and_run(&Trace::new(16, 4), 1, clif::Build::new(clif::OptLevel::None, clif::DeoptState::Ssa)).unwrap();
        let b = clif::compile_and_run(&Trace::new(64, 4), 1, clif::Build::new(clif::OptLevel::None, clif::DeoptState::Ssa)).unwrap();
        assert!(b.code_bytes > a.code_bytes * 2, "{} {}", a.code_bytes, b.code_bytes);
    }

    #[test]
    fn the_llvm_text_has_one_guard_body_and_cold_block_per_operation() {
        let ir = llvm::emit_ir(&small());
        for k in 0..16 {
            assert!(ir.contains(&format!("\nguard.{k}:")), "guard.{k}");
            assert!(ir.contains(&format!("\nbody.{k}:")), "body.{k}");
            assert!(ir.contains(&format!("\ncold.{k}:")), "cold.{k}");
        }
    }

    #[test]
    fn the_llvm_text_uses_the_same_operator_sequence_as_the_reference() {
        let t = small();
        let ir = llvm::emit_ir(&t);
        for (k, g) in t.ops.iter().enumerate() {
            let line = format!("%t.{k} = {} double %x.{k}, %y.{k}", g.op.llvm());
            assert!(ir.contains(&line), "missing {line}");
        }
    }

    #[test]
    fn the_generated_driver_carries_the_expected_answer() {
        let t = small();
        let driver = llvm::emit_driver(&t, 500);
        let bits = trace::evaluate(&t, 500).to_bits();
        assert!(driver.contains(&format!("{bits}ULL")), "expected bits missing");
    }

    #[test]
    fn spilling_the_deopt_state_does_not_change_the_answer() {
        // The stack slot is an implementation detail of how the cold path reads
        // the accumulator. If it changed the result the whole comparison below
        // would be between two different programs.
        let t = small();
        for opt in [clif::OptLevel::None, clif::OptLevel::Speed] {
            let ssa = clif::compile_and_run(
                &t, 500, clif::Build::new(opt, clif::DeoptState::Ssa)).unwrap();
            let spilled = clif::compile_and_run(
                &t, 500, clif::Build::new(opt, clif::DeoptState::Spilled)).unwrap();
            assert_eq!(ssa.out.to_bits(), spilled.out.to_bits(), "{opt:?}");
            assert_eq!(spilled.out.to_bits(), trace::evaluate(&t, 500).to_bits());
        }
    }

    #[test]
    fn a_broken_shape_takes_the_cold_path_with_the_state_spilled_too() {
        // The spilled cold path reads the accumulator back out of the slot, so
        // it has its own way to be wrong, and it is the path that never runs.
        let t = small();
        let mut build = clif::Build::new(clif::OptLevel::Speed, clif::DeoptState::Spilled);
        build.break_shape = Some(1);
        let c = clif::compile_and_run(&t, 500, build).unwrap();
        assert_eq!(c.ret, 2);
    }

    #[test]
    fn holding_the_deopt_state_in_ssa_makes_the_optimizer_go_quadratic() {
        // The finding this experiment exists for. At `speed`, a cold block that
        // uses the accumulator as an SSA value gets the whole chain leading to
        // it rebuilt inside it, so instruction count grows with the square of
        // the trace and one block ends up holding two instructions per
        // operation. Routing the same value through a stack slot leaves the
        // cold blocks a fixed size and the total linear.
        // Checked at two sizes rather than one, because a single ratio only
        // says the SSA form is bigger. Doubling the trace has to roughly double
        // the ratio for the growth to be quadratic against linear.
        let counts = |ops: usize, state| {
            let t = Trace::new(ops, 8);
            let c = clif::compile_and_run(
                &t, 1, clif::Build::new(clif::OptLevel::Speed, state)).unwrap();
            (c.insts, c.biggest_block)
        };
        let (ssa_64, big_64) = counts(64, clif::DeoptState::Ssa);
        let (ssa_128, big_128) = counts(128, clif::DeoptState::Ssa);
        let (sp_64, sp_big) = counts(64, clif::DeoptState::Spilled);
        let (sp_128, _) = counts(128, clif::DeoptState::Spilled);

        let ratio_64 = ssa_64 as f64 / sp_64 as f64;
        let ratio_128 = ssa_128 as f64 / sp_128 as f64;
        assert!(ratio_128 > 1.5 * ratio_64, "{ratio_64} {ratio_128}");
        // Spilled stays linear: twice the trace, no more than twice the code.
        assert!(sp_128 < sp_64 * 5 / 2, "{sp_64} {sp_128}");
        // And the pile-up is in one block, growing with the trace, not spread out.
        assert!(big_128 > 2 * big_64 - 16, "{big_64} {big_128}");
        assert!(sp_big < 16, "{sp_big}");
    }

    #[test]
    fn asking_for_vcode_gets_vcode_and_not_asking_gets_none() {
        let t = small();
        let mut build = clif::Build::new(clif::OptLevel::None, clif::DeoptState::Ssa);
        assert!(clif::compile_and_run(&t, 1, build).unwrap().vcode.is_none());
        build.want_vcode = true;
        let text = clif::compile_and_run(&t, 1, build).unwrap().vcode.unwrap();
        assert!(text.contains("block0:"), "{text}");
    }

    #[test]
    fn parse_rejects_a_flag_without_a_value() {
        let argv = ["cranelift", "--ops"].map(String::from);
        assert!(parse(argv.into_iter()).is_err());
    }

    #[test]
    fn timing_is_off_unless_asked_for_and_takes_no_value() {
        let plain = ["cranelift", "--ops", "16"].map(String::from);
        assert!(!parse(plain.into_iter()).unwrap().1.timing);
        // Trailing, which is where a flag that wrongly demanded a value would
        // fail, and followed by another flag, which is where it would eat one.
        let trailing = ["cranelift", "--ops", "16", "--timing"].map(String::from);
        let (_, a) = parse(trailing.into_iter()).unwrap();
        assert!(a.timing && a.ops == 16);
        let middle = ["cranelift", "--timing", "--ops", "32"].map(String::from);
        let (_, a) = parse(middle.into_iter()).unwrap();
        assert!(a.timing && a.ops == 32);
    }

    #[test]
    fn the_pass_breakdown_names_the_passes_that_ran() {
        // Cheap insurance that `timing` is still a default feature of
        // cranelift-codegen. Without it `take_current` returns an empty table
        // and `--timing` would print a header and nothing else, which reads as
        // "no pass took any time" rather than as a missing feature.
        let t = small();
        clif::compile_and_run(&t, 1, clif::Build::new(clif::OptLevel::Speed, clif::DeoptState::Ssa)).unwrap();
        let report = cranelift_codegen::timing::take_current().to_string();
        assert!(report.contains("Register allocation"), "{report}");
        assert!(report.contains("VCode lowering"), "{report}");
    }
}
