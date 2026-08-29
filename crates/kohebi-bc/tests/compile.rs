//! Compilation tests.
//!
//! Every one asserts on the listing rather than on a `Vec<Instr>`, for the same
//! reason the HIR tests assert on printed HIR: a reviewer can check a listing
//! against what Python does, and cannot check a debug-printed enum against
//! anything.

use kohebi_bc::code::Module;
use kohebi_bc::{compile, print};
use kohebi_hir::lower_module;
use kohebi_parse::parse_module;

fn module(source: &str) -> Module {
    let tree = parse_module(source).expect("expected this to parse");
    let body = lower_module(&tree, "<test>").expect("expected this to lower");
    compile(&body)
}

/// The compiled module body, for a test about one body's own tables.
fn code(source: &str) -> std::rc::Rc<kohebi_bc::Code> {
    module(source).body
}

/// The listing for a module, with the header line dropped.
fn bc(source: &str) -> String {
    print(&module(source))
        .lines()
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The target of the jump on this line of a listing.
fn target(line: &str) -> usize {
    line.rsplit(' ')
        .next()
        .and_then(|number| number.parse().ok())
        .unwrap_or_else(|| panic!("no jump target on {line:?}"))
}

#[test]
fn a_body_ends_by_returning_none() {
    // Not a rule the interpreter remembers. Running off the end of a body is
    // returning `None`, and here it is as two instructions.
    assert_eq!(
        bc("pass\n"),
        "   0  const      r0, None\n\
         \x20  1  ret        r0"
    );
}

#[test]
fn a_module_level_assignment_goes_through_the_dict() {
    assert_eq!(
        bc("x = 1\n"),
        "   0  const      r0, 1\n\
         \x20  1  setglobal  x, r0\n\
         \x20  2  const      r0, None\n\
         \x20  3  ret        r0"
    );
}

// Registers

/// Sibling expressions reuse the same registers, which is what stops a frame
/// from growing with the length of a function rather than with its depth.
#[test]
fn a_finished_subexpression_gives_its_registers_back() {
    // Five: the result, one for each side of the multiplication, and two more
    // shared by `a`, `b`, `c` and `d`. Without the rewind it would be seven.
    assert_eq!(code("x = (a + b) * (c + d)\n").registers, 5);
    // Two statements doing the same work use the same registers twice.
    assert_eq!(code("f(1)\ng(2)\n").registers, 3);
}

/// The other half of the same rule, and the reason it is a rewind rather than a
/// free list. Nothing here is dead: `f` is looked up before `g` is called, so it
/// is still live when `h` runs, and the same goes the whole way down. Seven
/// registers is what left to right evaluation costs, and CPython pushes exactly
/// as much onto its stack for the same expression.
#[test]
fn a_nest_that_is_all_live_at_once_pays_for_all_of_it() {
    assert_eq!(code("f(g(h(1)))\n").registers, 7);
}

#[test]
fn an_operand_that_is_already_in_a_slot_is_read_where_it_is() {
    // `$0` and `$1` are slots, so the addition names them rather than copying
    // them somewhere first. Nothing in this listing is a move.
    let listing = bc("x = f() + (a and b)\n");
    assert!(listing.contains("binary     r2, r0 + r1"), "{listing}");
    assert!(!listing.contains("move"), "{listing}");
}

#[test]
fn an_augmented_assignment_stays_one_instruction() {
    let listing = bc("x = 1\nx += 2\n");
    assert!(listing.contains("inplace"), "{listing}");
}

// Branches

#[test]
fn an_if_with_no_else_jumps_straight_past_it() {
    assert_eq!(
        bc("if a:\n    b\n"),
        "   0  getglobal  r1, a\n\
         \x20  1  truthy     r0, r1\n\
         \x20  2  jumpf      r0, 4\n\
         \x20  3  getglobal  r0, b\n\
         \x20  4  const      r0, None\n\
         \x20  5  ret        r0"
    );
}

#[test]
fn an_if_with_an_else_jumps_over_it_at_the_end_of_the_then() {
    let listing = bc("if a:\n    b\nelse:\n    c\n");
    assert!(listing.contains("   2  jumpf      r0, 5"), "{listing}");
    assert!(listing.contains("   4  jump       6"), "{listing}");
}

/// The one place the compiler looks at the shape of a test rather than just
/// compiling it. Negating a boolean cannot run user code, so peeling the `not`
/// and turning the jump around is the same program with one fewer instruction.
#[test]
fn a_negated_test_turns_the_jump_around_instead() {
    let listing = bc("if not a:\n    b\n");
    assert!(listing.contains("jumpt"), "{listing}");
    assert!(!listing.contains("not "), "{listing}");
}

#[test]
fn not_as_a_value_is_still_an_instruction() {
    // Only a test position gets the jump turned around. Asked for the value,
    // the negation has to actually happen.
    assert!(bc("x = not a\n").contains("not        "));
}

/// The truth protocol is its own instruction and is never folded into a branch,
/// because `__bool__` is user code and a second place for it to run is a second
/// place for it to be wrong.
#[test]
fn a_truth_test_is_visible_in_the_listing() {
    assert!(bc("if a:\n    pass\n").contains("truthy"));
}

// Loops

#[test]
fn a_while_loop_re_evaluates_its_test_every_turn() {
    // The jump at the bottom goes to 0, which is above the test, so the test
    // runs again. A jump to 2 would run it once and then loop forever.
    assert_eq!(
        bc("while a:\n    b\n"),
        "   0  getglobal  r1, a\n\
         \x20  1  truthy     r0, r1\n\
         \x20  2  jumpf      r0, 5\n\
         \x20  3  getglobal  r0, b\n\
         \x20  4  jump       0\n\
         \x20  5  const      r0, None\n\
         \x20  6  ret        r0"
    );
}

#[test]
fn a_for_loop_calls_iter_once_and_next_inside() {
    assert_eq!(
        bc("for i in xs:\n    f(i)\n"),
        "   0  getglobal  r2, xs\n\
         \x20  1  iter       r0, r2\n\
         \x20  2  next       r1, r0\n\
         \x20  3  exhausted  r2, r1\n\
         \x20  4  jumpt      r2, 10\n\
         \x20  5  setglobal  i, r1\n\
         \x20  6  getglobal  r3, f\n\
         \x20  7  getglobal  r4, i\n\
         \x20  8  call       r2, r3(r4)\n\
         \x20  9  jump       2\n\
         \x20 10  const      r2, None\n\
         \x20 11  ret        r2"
    );
}

/// `continue` goes to the setup rather than to the test. A `for` loop that
/// skipped its setup would ask an iterator it never advanced whether it was
/// finished, and spin forever.
#[test]
fn continue_takes_the_next_step_before_asking_again() {
    let listing = bc("for i in xs:\n    continue\n");
    let lines: Vec<&str> = listing.lines().collect();
    let iter = lines
        .iter()
        .position(|line| line.contains("iter "))
        .expect("there is an iter");
    let next = lines
        .iter()
        .position(|line| line.contains("next "))
        .expect("there is a next");
    let back = target(
        lines
            .iter()
            .rev()
            .find(|l| l.contains("jump "))
            .expect("a jump"),
    );
    assert!(back > iter, "continue went back past iter: {listing}");
    assert_eq!(back, next, "continue skipped the setup: {listing}");
}

#[test]
fn break_leaves_and_skips_the_else_clause() {
    // The `else` clause is what a false test falls into, so leaving the loop
    // outright means jumping past it. That is the whole of what Python's loop
    // `else` means, and here it is one jump target.
    let listing = bc("while a:\n    break\nelse:\n    b\n");
    let lines: Vec<&str> = listing.lines().collect();
    let breaking = target(
        lines
            .iter()
            .find(|l| l.contains("  3  jump"))
            .expect("a break"),
    );
    assert!(
        lines[breaking].contains("const"),
        "break landed inside the else clause: {listing}"
    );
}

// Evaluation order

/// `a.b = c` reads `c` and then `a`. A property on `a` can tell the difference,
/// and so can a `NameError` when both are undefined, so it is written into the
/// walk rather than left to chance.
#[test]
fn an_assignment_reads_its_value_before_its_target() {
    assert_eq!(
        bc("a.b = c\n"),
        "   0  getglobal  r0, c\n\
         \x20  1  getglobal  r1, a\n\
         \x20  2  setattr    r1.b, r0\n\
         \x20  3  const      r0, None\n\
         \x20  4  ret        r0"
    );
}

#[test]
fn an_item_assignment_reads_value_then_object_then_index() {
    assert_eq!(
        bc("a[i] = v\n"),
        "   0  getglobal  r0, v\n\
         \x20  1  getglobal  r1, a\n\
         \x20  2  getglobal  r2, i\n\
         \x20  3  setitem    r1[r2], r0\n\
         \x20  4  const      r0, None\n\
         \x20  5  ret        r0"
    );
}

#[test]
fn a_dict_display_reads_each_key_before_its_value() {
    let listing = bc("x = {a: b}\n");
    let a = listing.find(", a").expect("a is loaded");
    let b = listing.find(", b").expect("b is loaded");
    assert!(a < b, "{listing}");
}

// Pools

#[test]
fn a_constant_used_twice_is_stored_once() {
    // Both `1`s and both `None`s, so four uses become two entries.
    assert_eq!(code("x = 1\ny = 1\n").consts.len(), 2);
}

#[test]
fn a_name_used_twice_is_stored_once() {
    assert_eq!(
        module("x = a\ny = a\n").names,
        vec!["a".into(), "x".into(), "y".into()]
    );
}

// Containers and the rest

#[test]
fn the_containers_each_have_an_instruction() {
    assert!(bc("x = [1, 2]\n").contains("list       r0, [r1, r2]"));
    assert!(bc("x = (1, 2)\n").contains("tuple      r0, (r1, r2)"));
    assert!(bc("x = {1, 2}\n").contains("set        r0, {r1, r2}"));
    assert!(bc("x = {**a}\n").contains("dict       r0, {**r1}"));
}

#[test]
fn a_missing_part_of_a_slice_prints_as_a_hole() {
    assert!(bc("x = a[::2]\n").contains("slice      r2, _:_:r3"));
}

#[test]
fn a_raise_carries_its_cause() {
    assert!(bc("raise A from B\n").contains("raise      r0 from r1"));
    assert_eq!(bc("raise\n").lines().next(), Some("   0  raise"));
}

#[test]
fn deleting_names_the_kind_of_place_it_is_deleting() {
    assert!(bc("del a.b\n").contains("delattr    r0.b"));
    assert!(bc("del a[i]\n").contains("delitem    r0[r1]"));
    assert!(bc("del a\n").contains("delglobal  a"));
}

/// Nothing in a compiled body should still be carrying the value a forward jump
/// holds before its target is known. A jump nobody patched would run to a wild
/// offset, and it would be the interpreter that found out.
#[test]
fn every_jump_in_every_shape_gets_patched() {
    let sources = [
        "if a:\n    b\n",
        "if a:\n    b\nelse:\n    c\n",
        "while a:\n    b\n",
        "while a:\n    break\nelse:\n    c\n",
        "for i in xs:\n    continue\n",
        "for i in xs:\n    break\nelse:\n    c\n",
        "while a:\n    while b:\n        break\n    break\n",
        "x = a and b or c\n",
        "x = a if b else c\n",
        "x = a < b < c < d\n",
    ];
    for source in sources {
        let listing = bc(source);
        assert!(
            !listing.contains(&u32::MAX.to_string()),
            "an unpatched jump in {source:?}:\n{listing}"
        );
    }
}

// Functions

#[test]
fn a_def_builds_a_function_and_stores_it() {
    assert_eq!(
        bc("def f():\n    return 1\n"),
        "   0  makefunc   r0, f\n   \
            1  setglobal  f, r0\n   \
            2  const      r0, None\n   \
            3  ret        r0\n\
         code f: 1 registers\n   \
            0  const      r0, 1\n   \
            1  ret        r0\n   \
            2  const      r0, None\n   \
            3  ret        r0"
    );
}

#[test]
fn defaults_are_evaluated_where_the_def_is_and_handed_to_it() {
    // Registers rather than constants, because a default is an arbitrary
    // expression this frame evaluates once. A hole is a keyword-only parameter
    // with no default, and it has to print or the ones after it would look
    // shifted along.
    assert!(bc("def f(a, b=g()):\n    pass\n").contains("makefunc   r0, f, r1"));
    assert!(bc("def f(*, a=1, b):\n    pass\n").contains("makefunc   r0, f, r1, _"));
}

#[test]
fn a_parameter_is_a_register_the_call_fills_in() {
    // The parameters are the low registers in the order `Params` says, so the
    // body reads them by number and never by name.
    assert!(bc("def f(a, b):\n    return b\n").contains("code f: 3 registers"));
    assert!(bc("def f(a, b):\n    return b\n").contains("   0  ret        r1"));
}

#[test]
fn a_nested_def_is_a_body_of_the_body_that_wrote_it() {
    let listing = bc("def outer():\n    def inner():\n        pass\n");
    assert!(listing.contains("code outer: 2 registers"));
    assert!(listing.contains("code inner: 1 registers"));
}

#[test]
fn every_body_in_a_module_shares_one_name_table() {
    // The `total` a function reads and the `total` the module writes have to be
    // the same index, or reading a global would be back to hashing a string.
    let module = module("total = 0\ndef f():\n    global total\n    total = 1\n");
    assert_eq!(module.names, vec!["total".into(), "f".into()]);
}

#[test]
fn a_captured_name_lives_in_a_cell_and_the_def_hands_the_cell_over() {
    // `cell r0` before anything else, because the name has to be a cell before
    // the first write to it, and `makefunc ... over r0` rather than a read of
    // r0, because what the closure gets is the binding and not the value.
    assert_eq!(
        bc("def counter():\n    n = 0\n    def bump():\n        nonlocal n\n        n = n + 1\n"),
        "   0  makefunc   r0, counter\n   \
            1  setglobal  counter, r0\n   \
            2  const      r0, None\n   \
            3  ret        r0\n\
         code counter: 3 registers\n   \
            0  cell       r0\n   \
            1  const      r2, 0\n   \
            2  storecell  r0, r2\n   \
            3  makefunc   r1, bump, over r0\n   \
            4  const      r2, None\n   \
            5  ret        r2\n\
         code bump: 4 registers, over r0\n   \
            0  loadcell   r2, r0\n   \
            1  const      r3, 1\n   \
            2  binary     r1, r2 + r3\n   \
            3  storecell  r0, r1\n   \
            4  const      r1, None\n   \
            5  ret        r1"
    );
}

#[test]
fn a_free_register_is_named_on_the_body_that_takes_it() {
    let module = module("def o():\n    x = 1\n    def i():\n        return x\n");
    let outer = &module.body.functions[0];
    assert!(outer.free.is_empty());
    let inner = &outer.functions[0];
    assert_eq!(inner.free, vec![kohebi_bc::code::Reg(0)]);
    // The name is still on the register, because an unbound one has to say
    // which variable it was.
    assert_eq!(inner.local_at(kohebi_bc::code::Reg(0)), "x");
}

// Comprehensions

#[test]
fn a_comprehension_appends_to_a_container_it_made_rather_than_calling_a_method() {
    // `append r2, r1` and not a `getattr` of `append` followed by a call. The
    // list in r2 was built two instructions earlier by this same body, so there
    // is no name involved and nothing a program could have shadowed.
    assert_eq!(
        bc("r = [x for x in xs]\n"),
        "   0  makefunc   r1, <listcomp>\n   \
            1  getglobal  r3, xs\n   \
            2  iter       r2, r3\n   \
            3  call       r0, r1(r2)\n   \
            4  setglobal  r, r0\n   \
            5  const      r0, None\n   \
            6  ret        r0\n\
         code <listcomp>: 6 registers\n   \
            0  list       r2, []\n   \
            1  iter       r3, r0\n   \
            2  next       r4, r3\n   \
            3  exhausted  r5, r4\n   \
            4  jumpt      r5, 8\n   \
            5  move       r1, r4\n   \
            6  append     r2, r1\n   \
            7  jump       2\n   \
            8  ret        r2\n   \
            9  const      r5, None\n  \
           10  ret        r5"
    );
}

#[test]
fn a_dict_comprehension_is_the_item_store_a_program_would_have_written() {
    // A third instruction alongside `append` and `insert` would do exactly what
    // `setitem` already does, so there is not one. The key is filled before the
    // value, which is the order the source writes them in.
    let listing = bc("r = {k: v for k, v in pairs}\n");
    assert!(
        listing.contains("   9  getitem    r2, r6[r7]\n"),
        "{listing}"
    );
    assert!(
        listing.contains("  10  setitem    r3[r1], r2\n"),
        "{listing}"
    );
    assert!(!listing.contains("entry"), "{listing}");
}

#[test]
fn a_set_comprehension_is_told_it_is_one_rather_than_working_it_out() {
    // The instruction differs from a list's, so nothing has to look at what is
    // in the register on every element to decide which of the two this is.
    let listing = bc("r = {x for x in xs}\n");
    assert!(listing.contains("   6  insert     r2, r1\n"), "{listing}");
    assert!(!listing.contains("append"), "{listing}");
}

#[test]
fn a_del_of_a_shared_name_empties_the_cell_rather_than_the_register() {
    let listing = bc("def o():\n    x = 1\n    del x\n    def i():\n        return x\n");
    assert!(listing.contains("clearcell  r0"), "{listing}");
    assert!(!listing.contains("dellocal"), "{listing}");
}
