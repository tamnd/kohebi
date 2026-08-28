//! Build the trace as Cranelift IR and JIT it.
//!
//! Three numbers come out of here and they are three different questions. How
//! long it takes to construct the IR is a cost we pay no matter which backend
//! we pick, so it is reported separately rather than folded into the compile
//! time. How long the backend takes is the latency that decides whether tier 2
//! can run on the same thread as the program. How fast the result runs is
//! whether tier 2 was worth entering at all.

use std::time::Instant;

use cranelift_codegen::Context;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{
    AbiParam, BlockArg, InstBuilder, MemFlagsData, StackSlotData, StackSlotKind, types,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};

use crate::trace::Trace;

/// The cold path a failed guard branches to.
///
/// A real one rebuilds an interpreter frame from a side table. This one only
/// has to be a call the compiler cannot see through, because what it costs the
/// backend is not what the function does, it is that every value live across
/// the guard has to be somewhere the call can leave alone.
extern "C" fn deopt_stub(id: i64, iter: i64, acc: f64) -> i64 {
    // Reachable only when a guard fails, which in a correct run never happens.
    // The `--deopt-at` mode makes it happen on purpose.
    std::hint::black_box((iter, acc));
    id + 1
}

pub struct Compiled {
    pub ir_ns: u128,
    pub compile_ns: u128,
    pub code_bytes: usize,
    pub run_ns: u128,
    pub out: f64,
    pub ret: i64,
    /// Instructions in the CLIF after Cranelift's own passes have run, and the
    /// size of the largest single block. Code size alone says a back end
    /// produced a lot of machine code; these say whether the instructions were
    /// there in the IR before lowering, and whether they piled up in one place.
    pub insts: usize,
    pub biggest_block: usize,
    /// Machine-level VCode, after register allocation, when it was asked for.
    /// Two of the four combinations of optimization level and deopt state run
    /// five times faster than the other two on nearly identical code sizes, and
    /// no amount of reasoning about the IR explains that. Reading the registers
    /// does.
    pub vcode: Option<String>,
}

/// The knobs that change what gets compiled, as opposed to how it is measured.
#[derive(Clone, Copy, Debug)]
pub struct Build {
    pub opt: OptLevel,
    pub state: DeoptState,
    /// Corrupt this object's shape id before the run, so the guard at the first
    /// operation touching it fails and the cold path is actually taken. Without
    /// it the deopt blocks are compiled on every run and entered on none, which
    /// is exactly where a bug lives a long time.
    pub break_shape: Option<i64>,
    pub want_vcode: bool,
}

impl Build {
    pub fn new(opt: OptLevel, state: DeoptState) -> Build {
        Build { opt, state, break_shape: None, want_vcode: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptLevel {
    None,
    Speed,
}

impl OptLevel {
    pub fn parse(s: &str) -> Option<OptLevel> {
        match s {
            "none" => Some(OptLevel::None),
            "speed" => Some(OptLevel::Speed),
            _ => None,
        }
    }

    fn flag(self) -> &'static str {
        match self {
            OptLevel::None => "none",
            OptLevel::Speed => "speed",
        }
    }
}

/// How the cold path gets hold of the state it has to hand to the runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeoptState {
    /// The cold block uses the SSA value directly. The obvious way to write it,
    /// and the way that lets an optimizer treat the value as pure and duplicate
    /// it into every cold block rather than keeping it live.
    Ssa,
    /// The hot path stores the state into an explicit stack slot and the cold
    /// block loads it back. This is what Cranelift's user stack maps require of
    /// a producer anyway, so it is not an extra cost invented for this
    /// experiment, it is the shape the real deopt layer has to take.
    Spilled,
}

impl DeoptState {
    pub fn parse(s: &str) -> Option<DeoptState> {
        match s {
            "ssa" => Some(DeoptState::Ssa),
            "spilled" => Some(DeoptState::Spilled),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            DeoptState::Ssa => "ssa",
            DeoptState::Spilled => "spilled",
        }
    }
}

/// Compile the trace, run it, and time each stage.
pub fn compile_and_run(trace: &Trace, iters: i64, b: Build) -> Result<Compiled, String> {
    let Build { opt, state, break_shape, want_vcode } = b;
    let mut flags = settings::builder();
    // A JIT writes into memory it just mapped and calls it directly, so there
    // is nothing to relocate and no reason to pay for position independence.
    flags.set("is_pic", "false").map_err(|e| e.to_string())?;
    flags
        .set("use_colocated_libcalls", "false")
        .map_err(|e| e.to_string())?;
    flags
        .set("opt_level", opt.flag())
        .map_err(|e| e.to_string())?;

    let isa = cranelift_native::builder()
        .map_err(|e| e.to_string())?
        .finish(settings::Flags::new(flags))
        .map_err(|e| e.to_string())?;

    let mut jit = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    jit.symbol("deopt", deopt_stub as *const u8);
    let mut module = JITModule::new(jit);

    let mut deopt_sig = module.make_signature();
    deopt_sig.params.push(AbiParam::new(types::I64));
    deopt_sig.params.push(AbiParam::new(types::I64));
    deopt_sig.params.push(AbiParam::new(types::F64));
    deopt_sig.returns.push(AbiParam::new(types::I64));
    let deopt_id = module
        .declare_function("deopt", Linkage::Import, &deopt_sig)
        .map_err(|e| e.to_string())?;

    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64)); // slots
    sig.params.push(AbiParam::new(types::I64)); // out
    sig.params.push(AbiParam::new(types::I64)); // iters
    sig.returns.push(AbiParam::new(types::I64));
    let func_id = module
        .declare_function("trace", Linkage::Export, &sig)
        .map_err(|e| e.to_string())?;

    let mut ctx = Context::new();
    ctx.func.signature = sig;
    ctx.set_disasm(want_vcode);
    let mut fb_ctx = FunctionBuilderContext::new();

    let started = Instant::now();
    build(trace, &mut ctx, &mut fb_ctx, &mut module, deopt_id, state);
    let ir_ns = started.elapsed().as_nanos();

    let started = Instant::now();
    module
        .define_function(func_id, &mut ctx)
        .map_err(|e| e.to_string())?;
    // JITModule does not hand back a length for a finalized function, so read
    // the size off the compiled code while the context still holds it, before
    // `clear_context` throws it away.
    let code_bytes = ctx.compiled_code().map_or(0, |c| c.code_buffer().len());
    let vcode = ctx.compiled_code().and_then(|c| c.vcode.clone());
    // `ctx.func` at this point is what Cranelift's own passes left behind, so
    // counting here says how much of the code size was decided in the IR rather
    // than during lowering or spilling.
    let mut insts = 0usize;
    let mut biggest_block = 0usize;
    for block in ctx.func.layout.blocks() {
        let n = ctx.func.layout.block_insts(block).count();
        insts += n;
        biggest_block = biggest_block.max(n);
    }
    module.clear_context(&mut ctx);
    module
        .finalize_definitions()
        .map_err(|e| format!("{e:?}"))?;
    let compile_ns = started.elapsed().as_nanos();

    let ptr = module.get_finalized_function(func_id);
    let entry: extern "C" fn(*mut i64, *mut f64, i64) -> i64 =
        // SAFETY: `ptr` is the entry point Cranelift just compiled and
        // finalized for `func_id`, and the signature below is the one declared
        // for it above, three integer-sized arguments and an integer result.
        unsafe { std::mem::transmute(ptr) };

    let mut words = crate::trace::heap(trace.objects);
    if let Some(obj) = break_shape {
        words[(obj * crate::trace::OBJ_WORDS) as usize] = -1;
    }
    // The pointer table the compiled trace indexes, one entry per operation.
    let table: Vec<*mut i64> = crate::trace::offsets(trace)
        .iter()
        // SAFETY: every offset is `obj * OBJ_BYTES` for an `obj` below
        // `trace.objects`, and `words` is `objects * OBJ_WORDS` words long, so
        // each of these lands on an object header inside the allocation.
        .map(|off| unsafe { words.as_mut_ptr().byte_offset(*off as isize) })
        .collect();
    let mut out = 0.0f64;

    let started = Instant::now();
    let ret = entry(table.as_ptr().cast_mut().cast(), &raw mut out, iters);
    let run_ns = started.elapsed().as_nanos();

    // SAFETY: nothing compiled by this module is reachable after this point.
    // `entry` and `ptr` are not used again, and `words` is owned here.
    unsafe { module.free_memory() };

    Ok(Compiled {
        ir_ns,
        compile_ns,
        code_bytes,
        run_ns,
        out,
        ret,
        insts,
        biggest_block,
        vcode,
    })
}

fn build(
    trace: &Trace,
    ctx: &mut Context,
    fb_ctx: &mut FunctionBuilderContext,
    module: &mut JITModule,
    deopt_id: cranelift_module::FuncId,
    state: DeoptState,
) {
    let frontend_config = module.target_config();
    let ptr_ty = frontend_config.pointer_type();
    let deopt = module.declare_func_in_func(deopt_id, &mut ctx.func);
    let slot = match state {
        DeoptState::Ssa => None,
        DeoptState::Spilled => Some(ctx.func.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            8,
            3,
        ))),
    };
    let mut b = FunctionBuilder::new(&mut ctx.func, fb_ctx);

    // Only the loop head carries block arguments, matching the LLVM emitter,
    // which needs exactly two phi nodes and no more. Everywhere else a value is
    // used from the block that dominates its use, which is what both IRs allow
    // and what a front end that has already run its own SSA construction would
    // produce. Passing `i` and `acc` down a chain of a thousand blocks would
    // have given Cranelift more work than LLVM and called it a fair race.
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    let head = b.create_block();
    b.append_block_param(head, types::I64); // i
    b.append_block_param(head, types::F64); // acc
    let latch = b.create_block();
    let done = b.create_block();

    // Three blocks per operation. `guard` loads the shape word and branches,
    // `body` does the arithmetic on the hot side, `cold` calls out on the other.
    let ops: Vec<_> = (0..trace.ops.len())
        .map(|_| (b.create_block(), b.create_block(), b.create_block()))
        .collect();

    b.switch_to_block(entry);
    let params = b.block_params(entry).to_vec();
    let (slots, out_ptr, iters) = (params[0], params[1], params[2]);
    let zero_i = b.ins().iconst(types::I64, 0);
    let zero_f = b.ins().f64const(0.0);
    if let Some(slot) = slot {
        // The slot has to hold the state the first guard would deopt with,
        // which before any operation has run is the initial accumulator.
        b.ins().stack_store(ptr_ty, zero_f, slot, 0);
    }
    b.ins()
        .jump(head, &[BlockArg::Value(zero_i), BlockArg::Value(zero_f)]);

    b.switch_to_block(head);
    let i = b.block_params(head)[0];
    let entry_acc = b.block_params(head)[1];
    let more = b.ins().icmp(IntCC::SignedLessThan, i, iters);
    let first = ops.first().map_or(latch, |o| o.0);
    b.ins().brif(more, first, &[], done, &[]);

    // Aligned and non-trapping. The heap is ours and the offsets are constants,
    // so a bounds or alignment check here would be measuring a check no real
    // tier 2 would emit at this point either.
    let flags = MemFlagsData::trusted();
    let mut acc = entry_acc;
    for (k, g) in trace.ops.iter().enumerate() {
        let (guard, body, cold) = ops[k];
        let acc_in = acc;

        b.switch_to_block(guard);
        // `slots` is a table of pointers, one per operation, the way a Python
        // list holds references rather than objects. Loading the pointer first
        // is one extra load per operation and it is the load that makes the
        // measurement honest, because it stops a compiler proving that the
        // stores below do not alias the loads above.
        let base = b.ins().load(types::I64, flags, slots, (k as i32) * 8);
        let shape = b.ins().load(types::I64, flags, base, 0);
        let expect = b.ins().iconst(types::I64, g.shape_id);
        let ok = b.ins().icmp(IntCC::Equal, shape, expect);
        b.ins().brif(ok, body, &[], cold, &[]);

        b.switch_to_block(body);
        let x = b.ins().load(types::F64, flags, base, 8);
        let y = b.ins().load(types::F64, flags, base, 16);
        acc = emit_op(&mut b, g.op, acc_in, x, y);
        // Write the two fields back swapped. See `trace::evaluate` for why:
        // without a store that a later load might alias, the whole loop body is
        // loop invariant and an optimizing back end hoists it out, which turns
        // the run-time column into a measurement of how much of the program each
        // compiler managed to delete.
        b.ins().store(flags, y, base, 8);
        b.ins().store(flags, x, base, 16);
        if let Some(slot) = slot {
            // Published for the *next* guard, which is the one that would deopt
            // with this accumulator. One store per operation on the hot path,
            // which is the price of the cold path not holding the value.
            b.ins().stack_store(ptr_ty, acc, slot, 0);
        }
        let next = ops.get(k + 1).map_or(latch, |o| o.0);
        b.ins().jump(next, &[]);

        b.switch_to_block(cold);
        let id = b.ins().iconst(types::I64, k as i64);
        let live = match slot {
            None => acc_in,
            Some(slot) => b.ins().stack_load(ptr_ty, types::F64, slot, 0),
        };
        let call = b.ins().call(deopt, &[id, i, live]);
        let ret = b.inst_results(call)[0];
        b.ins().store(flags, live, out_ptr, 0);
        b.ins().return_(&[ret]);
    }

    b.switch_to_block(latch);
    let next_i = b.ins().iadd_imm_s(i, 1);
    b.ins()
        .jump(head, &[BlockArg::Value(next_i), BlockArg::Value(acc)]);

    b.switch_to_block(done);
    b.ins().store(flags, entry_acc, out_ptr, 0);
    let zero = b.ins().iconst(types::I64, 0);
    b.ins().return_(&[zero]);

    b.seal_all_blocks();
    b.finalize(frontend_config);
}

fn emit_op(
    b: &mut FunctionBuilder<'_>,
    op: crate::trace::Op,
    acc: cranelift_codegen::ir::Value,
    x: cranelift_codegen::ir::Value,
    y: cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    use crate::trace::Op;
    let combined = match op {
        Op::Add => b.ins().fadd(x, y),
        Op::Sub => b.ins().fsub(x, y),
        Op::Mul => b.ins().fmul(x, y),
        Op::Div => b.ins().fdiv(x, y),
    };
    b.ins().fadd(acc, combined)
}
