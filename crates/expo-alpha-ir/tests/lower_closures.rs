//! Coverage for the closure-lowering slice in `src/lower/closures.rs`
//! plus the matching `Resolution::Local` / fn-as-value dispatch in
//! `src/lower/calls.rs` and `src/lower/expr.rs`.
//!
//! Pinned shapes:
//!
//! - **Block / short closures** synthesize a
//!   `<enclosing>__closure<N>` body keyed on
//!   [`FunctionKind::Closure`] and emit
//!   [`IRInstruction::MakeClosure`] in the outer block.
//! - **Captures** populate `env_layout`, surface as
//!   [`IRInstruction::LoadCapture`] inside the body, and route
//!   through [`IRInstruction::MoveOutLocal`] when the outer slot is
//!   heap-typed and Owned.
//! - **Closure-typed local calls** lower to
//!   [`IRInstruction::CallClosure`] dispatching through a
//!   [`IRInstruction::LocalRead`] of the slot.
//! - **Fn-as-value adapters** synthesize one
//!   `<target>__as_closure` wrapper per named fn used as a value
//!   (cached across repeated references) and emit
//!   [`IRInstruction::MakeClosure`] with no captures.

use expo_alpha_ir::{FunctionKind, IRFunction, IRInstruction, IRProgram, IRType};
use expo_ast::util::dedent;

mod common;

use common::{function, lower_program_source as lower};

fn function_opt<'a>(program: &'a IRProgram, mangled: &str) -> Option<&'a IRFunction> {
    program.function(mangled)
}

fn require_synthesized<'a>(program: &'a IRProgram, mangled: &str) -> &'a IRFunction {
    function_opt(program, mangled).unwrap_or_else(|| {
        let names: Vec<_> = program
            .packages
            .iter()
            .flat_map(|p| p.functions.values().map(|f| f.symbol.mangled().to_string()))
            .collect();
        panic!("missing synthesized function `{mangled}` in IRProgram; have: {names:?}",);
    })
}

fn make_closure_in(function: &IRFunction) -> Vec<&IRInstruction> {
    function
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| matches!(i, IRInstruction::MakeClosure { .. }))
        .collect()
}

fn load_captures_in(function: &IRFunction) -> Vec<&IRInstruction> {
    function
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| matches!(i, IRInstruction::LoadCapture { .. }))
        .collect()
}

fn call_closures_in(function: &IRFunction) -> Vec<&IRInstruction> {
    function
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| matches!(i, IRInstruction::CallClosure { .. }))
        .collect()
}

fn move_out_locals_in(function: &IRFunction) -> Vec<&IRInstruction> {
    function
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| matches!(i, IRInstruction::MoveOutLocal { .. }))
        .collect()
}

fn int_fn_type(arity: usize) -> IRType {
    IRType::Function {
        params: vec![IRType::Int64; arity],
        ret: Box::new(IRType::Int64),
    }
}

#[test]
fn block_closure_without_captures_synthesizes_empty_env_body() {
    let source = "
        fn main -> Int
          f = fn (x: Int) -> Int
            x + 1
          end
          0
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");

    let body = require_synthesized(&program, "TestApp.main__closure0");
    let FunctionKind::Closure { env_layout } = &body.kind else {
        panic!(
            "synthesized closure body should be FunctionKind::Closure, got {:?}",
            body.kind
        );
    };
    assert!(
        env_layout.is_empty(),
        "captureless closure body should have an empty env_layout, got {env_layout:?}",
    );
    assert_eq!(body.params.len(), 1, "closure took one user-visible param");
    assert_eq!(body.params[0].ty, IRType::Int64);
    assert_eq!(body.return_type, IRType::Int64);

    let makes = make_closure_in(main);
    assert_eq!(makes.len(), 1, "exactly one MakeClosure in the outer fn");
    let IRInstruction::MakeClosure {
        body: body_symbol,
        captures,
        ty,
        ..
    } = makes[0]
    else {
        unreachable!()
    };
    assert_eq!(body_symbol.mangled(), "TestApp.main__closure0");
    assert!(captures.is_empty(), "no captures => empty captures vec");
    assert_eq!(*ty, int_fn_type(1));
}

#[test]
fn block_closure_with_capture_loads_through_env_layout() {
    let source = "
        fn main -> Int
          y = 10
          f = fn (x: Int) -> Int
            x + y
          end
          0
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    let body = require_synthesized(&program, "TestApp.main__closure0");

    let FunctionKind::Closure { env_layout } = &body.kind else {
        unreachable!("body kind already checked in the no-capture test")
    };
    assert_eq!(env_layout, &vec![IRType::Int64], "y is captured as Int64");

    let loads = load_captures_in(body);
    assert_eq!(loads.len(), 1, "body reads `y` once via LoadCapture");
    let IRInstruction::LoadCapture {
        capture_index, ty, ..
    } = loads[0]
    else {
        unreachable!()
    };
    assert_eq!(*capture_index, 0);
    assert_eq!(*ty, IRType::Int64);

    let makes = make_closure_in(main);
    let IRInstruction::MakeClosure { captures, .. } = makes[0] else {
        unreachable!()
    };
    assert_eq!(captures.len(), 1, "MakeClosure forwards one captured value");
}

#[test]
fn heap_typed_capture_moves_out_of_owner_slot() {
    let source = "
        fn main -> Int
          s = \"hi\"
          f = fn (x: Int) -> Int
            x
          end
          0
        end
        ";

    // `s` is unused inside the closure body, so it should NOT
    // appear in env_layout. We assert the negative shape: no
    // MoveOutLocal, no captures, and env_layout is empty. This
    // pins the capture-analysis dedup/visibility rules so a future
    // walker bug doesn't accidentally lift unused outer locals.
    let program = lower(&dedent(source));
    let body = require_synthesized(&program, "TestApp.main__closure0");
    let FunctionKind::Closure { env_layout } = &body.kind else {
        unreachable!("body kind already checked in the no-capture test")
    };
    assert!(
        env_layout.is_empty(),
        "unused outer locals are not captured"
    );
    let main = function(&program, "main");
    let makes = make_closure_in(main);
    let IRInstruction::MakeClosure { captures, .. } = makes[0] else {
        unreachable!()
    };
    assert!(captures.is_empty());
    assert!(
        move_out_locals_in(main).is_empty(),
        "no heap capture => no MoveOutLocal lifted into the env",
    );
}

#[test]
fn heap_capture_emits_move_out_local_into_env() {
    // Use `<>` to materialize `s` as a fresh, owned heap String so
    // its slot stamps `Ownership::Owned` (raw literals stamp
    // `Unowned` — they're static-data pointers). The closure
    // captures `s`, which routes the outer slot through
    // `MoveOutLocal` per the ownership-aware `read_capture` path.
    let source = "
        fn main -> Int
          s = \"hi\" <> \"there\"
          g = fn (x: Int) -> Int
            len(s) + x
          end
          0
        end

        fn len(s: String) -> Int
          0
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    let g_body = require_synthesized(&program, "TestApp.main__closure0");
    let FunctionKind::Closure { env_layout } = &g_body.kind else {
        unreachable!("g_body should be a closure")
    };
    assert_eq!(
        env_layout,
        &vec![IRType::String],
        "g captures `s: String` into env_layout[0]",
    );

    let moves = move_out_locals_in(main);
    assert_eq!(
        moves.len(),
        1,
        "outer `s` slot moves into the closure's env exactly once",
    );
    let IRInstruction::MoveOutLocal { ty, .. } = moves[0] else {
        unreachable!()
    };
    assert_eq!(*ty, IRType::String);
}

#[test]
fn closure_typed_local_call_lowers_to_call_closure() {
    let source = "
        fn main -> Int
          f = fn (x: Int) -> Int
            x + 1
          end
          f(5)
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");

    let calls = call_closures_in(main);
    assert_eq!(calls.len(), 1, "exactly one CallClosure for `f(5)`");
    let IRInstruction::CallClosure {
        args, result_ty, ..
    } = calls[0]
    else {
        unreachable!()
    };
    assert_eq!(args.len(), 1, "one user-visible arg");
    assert_eq!(*result_ty, IRType::Int64);

    // Also: no direct `Call` to a `__closure*` symbol — the indirect
    // path is the only dispatch for closure-typed locals.
    for inst in main.blocks.iter().flat_map(|b| &b.instructions) {
        if let IRInstruction::Call { callee, .. } = inst {
            assert!(
                !callee.mangled().contains("__closure"),
                "closure-typed local calls must not dispatch via direct Call (got `{}`)",
                callee.mangled(),
            );
        }
    }
}

#[test]
fn fn_as_value_synthesizes_captureless_wrapper_and_emits_make_closure() {
    let source = "
        fn add(x: Int, y: Int) -> Int
          x + y
        end

        fn apply(f: fn (Int, Int) -> Int, x: Int, y: Int) -> Int
          f(x, y)
        end

        fn main -> Int
          apply(add, 1, 2)
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    let wrapper = require_synthesized(&program, "TestApp.add__as_closure");

    let FunctionKind::Closure { env_layout } = &wrapper.kind else {
        panic!(
            "fn-as-value wrapper should be FunctionKind::Closure, got {:?}",
            wrapper.kind
        );
    };
    assert!(env_layout.is_empty(), "fn-as-value wrapper carries no env");
    assert_eq!(wrapper.params.len(), 2);
    assert_eq!(wrapper.return_type, IRType::Int64);

    let inner_calls: Vec<_> = wrapper
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter_map(|i| match i {
            IRInstruction::Call { callee, .. } => Some(callee.mangled()),
            _ => None,
        })
        .collect();
    assert_eq!(
        inner_calls,
        vec!["TestApp.add"],
        "wrapper body forwards directly to the wrapped fn",
    );

    let makes = make_closure_in(main);
    assert_eq!(
        makes.len(),
        1,
        "exactly one MakeClosure for the fn-as-value adapter site",
    );
    let IRInstruction::MakeClosure {
        body: body_symbol,
        captures,
        ty,
        ..
    } = makes[0]
    else {
        unreachable!()
    };
    assert_eq!(body_symbol.mangled(), "TestApp.add__as_closure");
    assert!(captures.is_empty());
    assert_eq!(*ty, int_fn_type(2));
}

#[test]
fn fn_as_value_wrapper_is_cached_across_repeated_references() {
    let source = "
        fn add(x: Int, y: Int) -> Int
          x + y
        end

        fn apply(f: fn (Int, Int) -> Int, x: Int, y: Int) -> Int
          f(x, y)
        end

        fn main -> Int
          apply(add, 1, 2) + apply(add, 3, 4)
        end
        ";

    let program = lower(&dedent(source));
    require_synthesized(&program, "TestApp.add__as_closure");

    let wrapper_count: usize = program
        .packages
        .iter()
        .map(|p| {
            p.functions
                .values()
                .filter(|f| f.symbol.mangled() == "TestApp.add__as_closure")
                .count()
        })
        .sum();
    assert_eq!(
        wrapper_count, 1,
        "two `add` references must reuse a single wrapper, got {wrapper_count}",
    );

    let main = function(&program, "main");
    let makes = make_closure_in(main);
    assert_eq!(
        makes.len(),
        2,
        "each fn-as-value site still emits its own MakeClosure",
    );
}
