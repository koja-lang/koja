//! Struct construction, generic struct instantiation, and field
//! access (`FieldChain` for named-local roots, `FieldLoad` for
//! arbitrary operand roots).
//!
//! Maps to LANGUAGE.md "Types" -- struct sub-section.

mod common;

use common::{dedent, eval_entry};
use expo_ir_eval::Value;

#[test]
fn evaluates_field_chain_access() {
    // Single-hop and nested field access on a named local exercises
    // `IRInstruction::FieldChain` -- the codegen path for a `local.field`
    // expression where the chain root resolves to a named binding.
    let source = "
        struct Inner
          v: Int
        end

        struct Outer
          inner: Inner
          extra: Int
        end

        fn run -> Int
          o = Outer { inner: Inner { v: 40 }, extra: 2 }
          o.inner.v + o.extra
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(42));
}

#[test]
fn evaluates_field_load_from_call_result() {
    // Receiver is a call result, not a named local, so the lowerer
    // emits `IRInstruction::FieldLoad` (not `FieldChain`). Exercises
    // the operand-materialize-then-project path.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn make_point() -> Point
          Point { x: 7, y: 35 }
        end

        fn run -> Int
          make_point().x + make_point().y
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(42));
}

#[test]
fn evaluates_generic_struct_construction() {
    let source = "
        struct Pair<L, R>
          left: L
          right: R
        end

        fn run -> Pair<Int, Int>
          Pair { left: 3, right: 4 }
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    let Value::Struct(s) = value else {
        panic!("expected struct value, got {value:?}");
    };
    assert_eq!(s.mangled.as_str(), "__test__.Pair_$Int.Int$");
    assert_eq!(s.fields.len(), 2);
    assert_eq!(s.fields[0].0, "left");
    assert_eq!(s.fields[0].1, Value::Int(3));
    assert_eq!(s.fields[1].0, "right");
    assert_eq!(s.fields[1].1, Value::Int(4));
}

#[test]
fn evaluates_struct_construction() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn run -> Point
          Point { x: 3, y: 4 }
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    let Value::Struct(s) = value else {
        panic!("expected struct value, got {value:?}");
    };
    assert_eq!(s.mangled.as_str(), "__test__.Point");
    assert_eq!(s.fields.len(), 2);
    assert_eq!(s.fields[0].0, "x");
    assert_eq!(s.fields[0].1, Value::Int(3));
    assert_eq!(s.fields[1].0, "y");
    assert_eq!(s.fields[1].1, Value::Int(4));
}
