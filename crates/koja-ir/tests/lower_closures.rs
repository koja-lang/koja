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
//!   [`IRInstruction::LoadCapture`] inside the body, and copy into
//!   the env via a [`IRInstruction::LocalRead`] of the outer slot
//!   (value semantics, the outer binding stays live).
//! - **Closure-typed local calls** lower to
//!   [`IRInstruction::CallClosure`] dispatching through a
//!   [`IRInstruction::LocalRead`] of the slot.
//! - **Fn-as-value adapters** synthesize one
//!   `<target>__as_closure` wrapper per named fn used as a value
//!   (cached across repeated references) and emit
//!   [`IRInstruction::MakeClosure`] with no captures.

use koja_ir::{FunctionKind, IRBasicBlock, IRFunction, IRInstruction, IRType};

mod common;

use common::{
    all_instructions, lower_script_source as lower, mangled_function, script_function_names,
};

fn make_closure_in(blocks: &[IRBasicBlock]) -> Vec<&IRInstruction> {
    all_instructions(blocks)
        .filter(|i| matches!(i, IRInstruction::MakeClosure { .. }))
        .collect()
}

fn load_captures_in(function: &IRFunction) -> Vec<&IRInstruction> {
    all_instructions(&function.blocks)
        .filter(|i| matches!(i, IRInstruction::LoadCapture { .. }))
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
        f = fn (x: Int) -> Int
          x + 1
        end
        0
        ";

    let script = lower(source);

    let body = mangled_function(&script, "TestApp.__script_body__closure0");
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

    let makes = make_closure_in(&script.blocks);
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
    assert_eq!(body_symbol.mangled(), "TestApp.__script_body__closure0");
    assert!(captures.is_empty(), "no captures => empty captures vec");
    assert_eq!(*ty, int_fn_type(1));
}

#[test]
fn block_closure_with_capture_loads_through_env_layout() {
    let source = "
        y = 10
        f = fn (x: Int) -> Int
          x + y
        end
        0
        ";

    let script = lower(source);
    let body = mangled_function(&script, "TestApp.__script_body__closure0");

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

    let makes = make_closure_in(&script.blocks);
    let IRInstruction::MakeClosure { captures, .. } = makes[0] else {
        unreachable!()
    };
    assert_eq!(captures.len(), 1, "MakeClosure forwards one captured value");
}

#[test]
fn unused_outer_local_is_not_captured() {
    let source = "
        s = \"hi\"
        f = fn (x: Int) -> Int
          x
        end
        0
        ";

    // `s` is unused inside the closure body, so it should NOT appear
    // in env_layout or carry a capture value. Pins the
    // capture-analysis dedup/visibility rules so a future walker bug
    // doesn't accidentally lift unused outer locals.
    let script = lower(source);
    let body = mangled_function(&script, "TestApp.__script_body__closure0");
    let FunctionKind::Closure { env_layout } = &body.kind else {
        unreachable!("body kind already checked in the no-capture test")
    };
    assert!(
        env_layout.is_empty(),
        "unused outer locals are not captured"
    );
    let makes = make_closure_in(&script.blocks);
    let IRInstruction::MakeClosure { captures, .. } = makes[0] else {
        unreachable!()
    };
    assert!(captures.is_empty());
}

#[test]
fn match_arm_binding_inside_closure_is_not_captured() {
    // `n` is bound by the arm pattern *inside* the closure, so it
    // must lower as a body local, not a capture of the enclosing
    // function. The enclosing function never declared the slot, and
    // misclassifying ICEs at seal.
    let source = "
        f = fn () -> Int
          o = Option.Some(3)
          match o
            Option.Some(n) -> n
            Option.None -> 0
          end
        end
        0
        ";

    let script = lower(source);
    let body = mangled_function(&script, "TestApp.__script_body__closure0");
    let FunctionKind::Closure { env_layout } = &body.kind else {
        unreachable!("body kind already checked in the no-capture test")
    };
    assert!(
        env_layout.is_empty(),
        "arm bindings are body locals, not captures: {env_layout:?}",
    );
    assert!(load_captures_in(body).is_empty());
}

#[test]
fn nested_block_assignment_inside_closure_is_not_captured() {
    // `y` is first assigned inside an `if` arm within the closure.
    // The capture walker must see nested assignments, not just the
    // body's top-level statements.
    let source = "
        f = fn () -> Int
          if true
            y = 2
            y
          else
            0
          end
        end
        0
        ";

    let script = lower(source);
    let body = mangled_function(&script, "TestApp.__script_body__closure0");
    let FunctionKind::Closure { env_layout } = &body.kind else {
        unreachable!("body kind already checked in the no-capture test")
    };
    assert!(
        env_layout.is_empty(),
        "nested-block locals are body locals, not captures: {env_layout:?}",
    );
    assert!(load_captures_in(body).is_empty());
}

#[test]
fn heap_capture_copies_into_env_via_local_read() {
    // The closure captures heap-typed `s`. Under value semantics the
    // capture copies into the env via a `LocalRead` of the outer slot
    // (the binding stays live). There is no move-out.
    let source = "
        s = \"hi\" <> \"there\"
        g = fn (x: Int) -> Int
          len(s) + x
        end
        0

        fn len(s: String) -> Int
          0
        end
        ";

    let script = lower(source);
    let g_body = mangled_function(&script, "TestApp.__script_body__closure0");
    let FunctionKind::Closure { env_layout } = &g_body.kind else {
        unreachable!("g_body should be a closure")
    };
    assert_eq!(
        env_layout,
        &vec![IRType::String],
        "g captures `s: String` into env_layout[0]",
    );

    // The captured value forwarded to MakeClosure is read from the
    // outer slot, not moved out of it.
    let makes = make_closure_in(&script.blocks);
    let IRInstruction::MakeClosure { captures, .. } = makes[0] else {
        unreachable!()
    };
    assert_eq!(captures.len(), 1, "g forwards one captured value");
    let reads_string = all_instructions(&script.blocks)
        .any(|i| matches!(i, IRInstruction::LocalRead { ty, .. } if *ty == IRType::String));
    assert!(
        reads_string,
        "the heap capture copies into the env via a String LocalRead",
    );
}

#[test]
fn closure_typed_local_call_lowers_to_call_closure() {
    let source = "
        f = fn (x: Int) -> Int
          x + 1
        end
        f(5)
        ";

    let script = lower(source);

    let calls: Vec<_> = all_instructions(&script.blocks)
        .filter(|i| matches!(i, IRInstruction::CallClosure { .. }))
        .collect();
    assert_eq!(calls.len(), 1, "exactly one CallClosure for `f(5)`");
    let IRInstruction::CallClosure {
        args, result_ty, ..
    } = calls[0]
    else {
        unreachable!()
    };
    assert_eq!(args.len(), 1, "one user-visible arg");
    assert_eq!(*result_ty, IRType::Int64);

    // Also assert there is no direct `Call` to a `__closure*` symbol.
    // The indirect path is the only dispatch for closure-typed locals.
    for inst in all_instructions(&script.blocks) {
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

        apply(add, 1, 2)
        ";

    let script = lower(source);
    let wrapper = mangled_function(&script, "TestApp.add__as_closure");

    let FunctionKind::Closure { env_layout } = &wrapper.kind else {
        panic!(
            "fn-as-value wrapper should be FunctionKind::Closure, got {:?}",
            wrapper.kind
        );
    };
    assert!(env_layout.is_empty(), "fn-as-value wrapper carries no env");
    assert_eq!(wrapper.params.len(), 2);
    assert_eq!(wrapper.return_type, IRType::Int64);

    let inner_calls: Vec<_> = all_instructions(&wrapper.blocks)
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

    let makes = make_closure_in(&script.blocks);
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

        apply(add, 1, 2) + apply(add, 3, 4)
        ";

    let script = lower(source);

    let wrapper_count = script_function_names(&script)
        .iter()
        .filter(|name| *name == "TestApp.add__as_closure")
        .count();
    assert_eq!(
        wrapper_count, 1,
        "two `add` references must reuse a single wrapper, got {wrapper_count}",
    );

    let makes = make_closure_in(&script.blocks);
    assert_eq!(
        makes.len(),
        2,
        "each fn-as-value site still emits its own MakeClosure",
    );
}
