//! Conditional conformance: `impl P for T<X: Bound>` records its
//! bounds on the `Parameterized` scope and discharges them per
//! instantiation at resolve time.
//!
//! Pins:
//! - the recorded fact carries per-slot bounds
//! - bound discharge accepts satisfying instantiations, rejects the
//!   rest, and recurses through nesting
//! - generic code threads into a conditional impl only with the
//!   declared bound
//! - the impl body dispatches on its own condition (the overlay)
//! - `==` consults the conditional `Equality` fact, so lists of
//!   closures diagnose instead of reaching monomorphization
//! - bounds on non-param target args are rejected at lift

use koja_ast::util::dedent;
use koja_typecheck::ConformanceScope;

mod common;

use common::{
    PACKAGE, assert_script_fails_with, global_id, global_leaf, global_named, registry_id,
    typecheck_script,
};

/// The shared fixture: a codec protocol, a `String` impl, and a
/// conditional `List` impl whose body dispatches through the
/// impl's own bound.
const CODEC: &str = "
    protocol Encodable
      fn to_wire(self) -> String
    end

    impl Encodable for String
      fn to_wire(self) -> String
        self
      end
    end

    impl Encodable for List<T: Encodable>
      fn to_wire(self) -> String
        result = \"[\"

        for item in self
          result = result <> item.to_wire()
        end

        result <> \"]\"
      end
    end

    fn render<T: Encodable>(value: T) -> String
      value.to_wire()
    end
";

fn codec_script(tail: &str) -> String {
    format!("{CODEC}\n{tail}\n")
}

#[test]
fn conditional_fact_records_per_slot_bounds() {
    let checked = typecheck_script(&dedent(&codec_script("1.print()")));
    let list_id = global_id(&checked, "List");
    let encodable_id = registry_id(&checked, PACKAGE, &["Encodable"]);
    let records = checked
        .registry
        .conformance_records(list_id, encodable_id)
        .expect("List should carry an Encodable record");
    assert_eq!(records.len(), 1);
    let ConformanceScope::Parameterized { bounds } = &records[0].scope else {
        panic!("conditional impl should record Parameterized, got {records:?}");
    };
    assert_eq!(bounds.len(), 1);
    assert_eq!(bounds[0].len(), 1);
    assert_eq!(bounds[0][0].protocol_id, encodable_id);
    assert!(bounds[0][0].args.is_empty());
}

#[test]
fn conditional_fact_matches_only_satisfying_instantiations() {
    let checked = typecheck_script(&dedent(&codec_script("1.print()")));
    let list_id = global_id(&checked, "List");
    let encodable_id = registry_id(&checked, PACKAGE, &["Encodable"]);
    assert!(
        checked
            .registry
            .lookup_conformance(list_id, encodable_id, &[global_leaf(&checked, "String")])
            .is_some(),
        "`List<String>` should satisfy the conditional fact",
    );
    assert!(
        checked
            .registry
            .lookup_conformance(list_id, encodable_id, &[global_leaf(&checked, "Int")])
            .is_none(),
        "`List<Int>` should not satisfy the conditional fact",
    );
    let nested = global_named(&checked, "List", vec![global_leaf(&checked, "String")]);
    assert!(
        checked
            .registry
            .lookup_conformance(list_id, encodable_id, &[nested])
            .is_some(),
        "`List<List<String>>` should satisfy the conditional fact recursively",
    );
}

#[test]
fn discharge_accepts_satisfying_instantiation() {
    typecheck_script(&dedent(&codec_script("render([\"a\", \"b\"]).print()")));
}

#[test]
fn discharge_recurses_through_nesting() {
    typecheck_script(&dedent(&codec_script("render([[\"a\"], [\"b\"]]).print()")));
}

#[test]
fn discharge_rejects_unsatisfying_instantiation() {
    assert_script_fails_with(
        &codec_script("render([1, 2]).print()"),
        &["does not implement protocol `Encodable`"],
    );
}

#[test]
fn generic_threading_requires_the_declared_bound() {
    typecheck_script(&dedent(&codec_script(
        "
        fn wrap<U: Encodable>(xs: List<U>) -> String
          render(xs)
        end

        wrap([\"a\"]).print()
        ",
    )));
    assert_script_fails_with(
        &codec_script(
            "
            fn wrap<U>(xs: List<U>) -> String
              render(xs)
            end

            wrap([\"a\"]).print()
            ",
        ),
        &["does not implement protocol `Encodable`"],
    );
}

#[test]
fn equality_gate_rejects_function_element_lists() {
    assert_script_fails_with(
        "
        f = fn () -> Int 1 end
        g = fn () -> Int 2 end
        ([f] == [g]).print()
        ",
        &["does not implement `Equality`"],
    );
}

#[test]
fn equality_gate_accepts_nested_lists() {
    typecheck_script(&dedent(
        "
        ([[1, 2], [3]] == [[1, 2], [3]]).print()
        ",
    ));
}

#[test]
fn bound_on_concrete_target_arg_rejected() {
    assert_script_fails_with(
        "
        protocol Encodable
          fn to_wire(self) -> String
        end

        impl Encodable for List<Int: Encodable>
          fn to_wire(self) -> String
            \"nope\"
          end
        end

        1.print()
        ",
        &["must attach to one of the target's own type parameters"],
    );
}
