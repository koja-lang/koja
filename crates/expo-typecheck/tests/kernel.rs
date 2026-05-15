//! Surface-level coverage for the auto-imported `Global.kernel`
//! source. Pins that the typecheck registry stamps the expected
//! signatures for the public surface (`Kernel.panic`, `Option<T>`,
//! `Result<T, E>`, `Pair<A, B>`, `Range`, the `Equality` / `Hash`
//! protocol impls, `Int.parse` / `Float.parse`, and the `Binary` /
//! `Bits` conversion intrinsics) and that user code can call into
//! them without the autoimport raising diagnostics.
//!
//! `Random` lives in `random.expo` and is auto-imported alongside
//! the rest of the alpha stdlib; surface coverage for it lives in
//! [`random`].

use expo_alpha_typecheck::{CheckedProgram, GlobalKind};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::util::dedent;

mod common;

use common::typecheck_file as typecheck;

fn assert_registered(checked: &CheckedProgram, segments: &[&str]) {
    let id = Identifier::new("Global", segments.iter().map(|s| s.to_string()).collect());
    assert!(
        checked.registry.lookup(&id).is_some(),
        "expected `{id}` to be registered after autoimport",
    );
}

#[test]
fn kernel_panic_registers_with_never_return() {
    let checked = typecheck("fn main\n  1\nend\n");
    let id = Identifier::new("Global", vec!["Kernel".to_string(), "panic".to_string()]);
    let (_, entry) = checked
        .registry
        .lookup(&id)
        .expect("Kernel.panic should be registered");
    let GlobalKind::Function(Some(signature)) = &entry.kind else {
        panic!(
            "Kernel.panic should carry a stamped signature; got {:?}",
            entry.kind
        );
    };
    // The lift_signatures override rewrites the source's `Unit`
    // return into `Global.Never` so callers in match-arm tail
    // position propagate the surrounding arm's expected type instead
    // of mismatching against `Unit`.
    let never_id = Identifier::new("Global", vec!["Never".to_string()]);
    let (expected_id, _) = checked
        .registry
        .lookup(&never_id)
        .expect("Global.Never must register before Kernel.panic can target it");
    let ResolvedType::Named {
        resolution,
        type_args,
    } = &signature.return_type
    else {
        panic!(
            "Kernel.panic return should be a Named type pointing at `Never`; got {:?}",
            signature.return_type,
        );
    };
    assert!(
        type_args.is_empty(),
        "`Never` is nullary; got type_args={type_args:?}",
    );
    assert_eq!(
        resolution,
        &Resolution::Global(expected_id),
        "Kernel.panic return should resolve to `Global.Never`; got {resolution:?}",
    );
}

#[test]
fn kernel_panic_callable_in_arm_tail_with_polymorphic_return() {
    // `Option.unwrap` exercises the bidirectional propagation:
    // `Kernel.panic(...)` in the `None` arm tail must fit the `T`
    // expected by the surrounding match. Compiles only if the
    // `Never` rewrite + bidirectional inference are both wired.
    let checked = typecheck(&dedent(
        "
        fn main -> Int
          Option.Some(7).unwrap()
        end
        ",
    ));
    let main = Identifier::new("TestApp", vec!["main".to_string()]);
    assert!(
        checked.registry.lookup(&main).is_some(),
        "TestApp.main should typecheck end-to-end",
    );
}

#[test]
fn option_result_pair_range_register_after_autoimport() {
    let checked = typecheck("fn main\n  1\nend\n");
    assert_registered(&checked, &["Option"]);
    assert_registered(&checked, &["Result"]);
    assert_registered(&checked, &["Pair"]);
    assert_registered(&checked, &["Range"]);
}

#[test]
fn random_registers_after_autoimport() {
    let checked = typecheck("fn main\n  1\nend\n");
    let id = Identifier::new("Global", vec!["Random".to_string()]);
    assert!(
        checked.registry.lookup(&id).is_some(),
        "`Random` lives in `random.expo` and is auto-imported alongside the \
         rest of the alpha stdlib; its `bytes` body resolves through \
         `String.to_binary`, defined in `string.expo`",
    );
}

#[test]
fn equality_eq_registers_for_bool_and_each_int_width() {
    let checked = typecheck("fn main\n  1\nend\n");
    for receiver in [
        "Bool", "Int", "Int8", "Int16", "Int32", "UInt8", "UInt16", "UInt32", "UInt64",
    ] {
        assert_registered(&checked, &[receiver, "eq"]);
    }
}

#[test]
fn hash_hash_registers_for_bool_and_each_int_width() {
    let checked = typecheck("fn main\n  1\nend\n");
    for receiver in [
        "Bool", "Int", "Int8", "Int16", "Int32", "UInt8", "UInt16", "UInt32", "UInt64",
    ] {
        assert_registered(&checked, &[receiver, "hash"]);
    }
}

#[test]
fn int_parse_and_float_parse_register_with_result_returns() {
    let checked = typecheck("fn main\n  1\nend\n");
    assert_registered(&checked, &["Int", "parse"]);
    assert_registered(&checked, &["Float", "parse"]);
}

#[test]
fn binary_intrinsics_register() {
    let checked = typecheck("fn main\n  1\nend\n");
    for method in ["byte_size", "ptr", "to_bits", "to_string"] {
        assert_registered(&checked, &["Binary", method]);
    }
    assert_registered(&checked, &["Bits", "to_binary"]);
}

#[test]
fn user_code_can_call_eq_and_hash_through_method_chain() {
    typecheck(&dedent(
        "
        fn main -> Bool
          1.eq(1)
        end
        ",
    ));
    typecheck(&dedent(
        "
        fn main -> Int
          42.hash()
        end
        ",
    ));
}
