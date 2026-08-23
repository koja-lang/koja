//! Phase 2 typecheck coverage for type-parameter bounds:
//! parsing/lifting `<T: P>`, multi-bound `<T: P1 & P2>`, bound name
//! resolution, and call-site verification through
//! [`koja_typecheck::pipeline::resolve::types::verify_bounds`].
//!
//! Bound *dispatch* (calling a bound's method on a value typed as
//! the bounded param) lives in `bounded_dispatch.rs`. This file is
//! about the bound itself: that the registry remembers it, that
//! call-site inference enforces it, and that the diagnostic shape
//! matches LANGUAGE.md §10.

use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;

mod common;

use common::{
    PACKAGE, assert_script_fails_with, diagnostic_messages, typecheck_script as typecheck,
    typecheck_script_fail as typecheck_fail,
};

#[test]
fn single_bound_lifts_into_registry_type_param_bounds() {
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        fn show<T: Greeter>(value: T) -> String
          value.greet()
        end
        ";

    let checked = typecheck(&dedent(source));
    let (show_id, _) = checked
        .registry
        .lookup(&Identifier::new(PACKAGE, vec!["show".to_string()]))
        .expect("show exists");
    let bounds = checked
        .registry
        .type_param_bounds(show_id)
        .expect("show carries lifted bounds");
    assert_eq!(bounds.len(), 1);
    let (greeter_id, _) = checked
        .registry
        .lookup(&Identifier::new(PACKAGE, vec!["Greeter".to_string()]))
        .expect("Greeter exists");
    assert_eq!(bounds[0].len(), 1);
    assert_eq!(bounds[0][0].protocol_id, greeter_id);
    assert!(bounds[0][0].args.is_empty());
}

#[test]
fn multi_bound_lifts_with_each_protocol_listed_in_order() {
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        protocol Shower
          fn show(self) -> String
        end

        fn render<T: Greeter & Shower>(value: T) -> String
          value.greet()
        end
        ";

    let checked = typecheck(&dedent(source));
    let (render_id, _) = checked
        .registry
        .lookup(&Identifier::new(PACKAGE, vec!["render".to_string()]))
        .expect("render exists");
    let bounds = checked
        .registry
        .type_param_bounds(render_id)
        .expect("render carries lifted bounds");
    assert_eq!(bounds.len(), 1);

    let (greeter_id, _) = checked
        .registry
        .lookup(&Identifier::new(PACKAGE, vec!["Greeter".to_string()]))
        .expect("Greeter exists");
    let (shower_id, _) = checked
        .registry
        .lookup(&Identifier::new(PACKAGE, vec!["Shower".to_string()]))
        .expect("Shower exists");
    assert_eq!(
        bounds[0]
            .iter()
            .map(|bound| bound.protocol_id)
            .collect::<Vec<_>>(),
        vec![greeter_id, shower_id],
    );
}

#[test]
fn unknown_bound_name_diagnoses_at_lift() {
    let source = "
        fn show<T: NotARealProtocol>(value: T) -> Int
          0
        end
        ";

    assert_script_fails_with(source, &["NotARealProtocol"]);
}

#[test]
fn non_protocol_bound_diagnoses_at_lift() {
    let source = "
        struct Point
          x: Int
        end

        fn show<T: Point>(value: T) -> Int
          0
        end
        ";

    assert_script_fails_with(source, &["Point"]);
}

#[test]
fn parameterized_bound_checks_protocol_arity() {
    let source = "
        protocol Source<T>
          fn first(self) -> T
        end

        fn read<E: Source>(source: E) -> Int
          0
        end
        ";

    assert_script_fails_with(source, &["expects 1 type argument", "got 0"]);
}

#[test]
fn call_site_with_unimplemented_bound_diagnoses() {
    // `Point` does not implement `Greeter`, so `show(Point{...})`
    // surfaces the LANGUAGE.md §10 bound-enforcement message.
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Point
          x: Int
        end

        fn show<T: Greeter>(value: T) -> String
          value.greet()
        end

        show(Point{x: 1})
        ";

    assert_script_fails_with(
        source,
        &[
            "does not implement protocol `Greeter`",
            "required by type parameter `T`",
        ],
    );
}

#[test]
fn tuple_call_site_with_custom_bound_diagnoses() {
    let source = "
        protocol Marked
          fn mark(self) -> Int
        end

        fn use_mark<T: Marked>(value: T) -> Int
          value.mark()
        end

        use_mark((1, 2))
        ";

    assert_script_fails_with(
        source,
        &[
            "does not implement protocol `Marked`",
            "required by type parameter `T`",
        ],
    );
}

#[test]
fn tuple_call_site_satisfies_structural_protocol_bounds() {
    let source = "
        fn render<T: Debug>(value: T) -> String
          value.format()
        end

        fn equal<T: Equality>(left: T, right: T) -> Bool
          left.equals?(right)
        end

        render((1, \"one\"))
        equal((1, 2), (1, 2))
        ";

    typecheck(&dedent(source));
}

#[test]
fn call_site_with_implemented_bound_succeeds() {
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Point
          x: Int
        end

        impl Greeter for Point
          fn greet(self) -> String
            \"Point\"
          end
        end

        fn show<T: Greeter>(value: T) -> String
          value.greet()
        end

        show(Point{x: 1})
        ";

    typecheck(&dedent(source));
}

#[test]
fn call_site_with_partial_multi_bound_diagnoses_missing_protocol() {
    // `Point` implements `Greeter` but not `Shower`. The
    // verify_bounds walk should still emit one diagnostic for the
    // missing `Shower` impl even though `Greeter` is satisfied.
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        protocol Shower
          fn show(self) -> String
        end

        struct Point
          x: Int
        end

        impl Greeter for Point
          fn greet(self) -> String
            \"Point\"
          end
        end

        fn render<T: Greeter & Shower>(value: T) -> String
          value.greet()
        end

        render(Point{x: 1})
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("does not implement protocol `Shower`")),
        "expected missing-Shower-bound diagnostic, got {messages:?}",
    );
    assert!(
        !messages
            .iter()
            .any(|m| m.contains("does not implement protocol `Greeter`")),
        "did not expect a Greeter diagnostic (Point implements Greeter); got {messages:?}",
    );
}

#[test]
fn call_site_threads_matching_bounded_param_into_bounded_call() {
    // `outer<U: Greeter>(u: U)` calls `inner<T: Greeter>(value: T)`
    // with `u`, threading a bounded type param into another generic.
    // The declared bounds match, so the call is valid.
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        fn inner<T: Greeter>(value: T) -> String
          value.greet()
        end

        fn outer<U: Greeter>(u: U) -> String
          inner(u)
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn threaded_type_parameter_requires_matching_protocol_arguments() {
    let source = "
        protocol Source<T>
          fn first(self) -> T
        end

        fn inner<E: Source<Int>>(source: E) -> Int
          source.first()
        end

        fn outer<E: Source<String>>(source: E) -> Int
          inner(source)
        end
        ";

    assert_script_fails_with(source, &["does not implement protocol `Source<Int>`"]);
}

#[test]
fn parameterized_bound_accepts_concrete_protocol_argument() {
    let source = "
        protocol Source<T>
          fn first(self) -> T
        end

        struct IntSource
          value: Int
        end

        impl Source<Int> for IntSource
          fn first(self) -> Int
            self.value
          end
        end

        fn read<E: Source<Int>>(source: E) -> Int
          source.first()
        end

        read(IntSource{value: 1})
        ";

    typecheck(&dedent(source));
}

#[test]
fn parameterized_bound_accepts_sibling_type_parameter_argument() {
    let source = "
        protocol Source<T>
          fn first(self) -> T
        end

        struct IntSource
          value: Int
        end

        impl Source<Int> for IntSource
          fn first(self) -> Int
            self.value
          end
        end

        fn read<T, E: Source<T>>(source: E, fallback: T) -> T
          source.first()
        end

        read(IntSource{value: 1}, 0)
        ";

    typecheck(&dedent(source));
}

#[test]
fn parameterized_bound_rejects_mismatched_protocol_argument() {
    let source = "
        protocol Source<T>
          fn first(self) -> T
        end

        struct StringSource
          value: String
        end

        impl Source<String> for StringSource
          fn first(self) -> String
            self.value
          end
        end

        fn read<E: Source<Int>>(source: E) -> Int
          source.first()
        end

        read(StringSource{value: \"no\"})
        ";

    assert_script_fails_with(
        source,
        &[
            "does not implement protocol `Source<Int>`",
            "required by type parameter `E`",
        ],
    );
}

#[test]
fn conditional_conformance_uses_parameterized_overlay() {
    let source = "
        protocol Source<T>
          fn first(self) -> T
        end

        protocol Marker<T>
          fn mark(self) -> T
        end

        struct Pair<T, E>
          fallback: T
          source: E
        end

        impl Marker<T> for Pair<T, E: Source<T>>
          fn mark(self) -> T
            self.source.first()
          end
        end

        struct IntSource
          value: Int
        end

        impl Source<Int> for IntSource
          fn first(self) -> Int
            self.value
          end
        end

        fn use_marker<X: Marker<Int>>(value: X) -> Int
          value.mark()
        end

        use_marker(Pair{fallback: 0, source: IntSource{value: 1}})
        ";

    typecheck(&dedent(source));
}

#[test]
fn conditional_conformance_rejects_mismatched_protocol_argument() {
    let source = "
        protocol Source<T>
          fn first(self) -> T
        end

        protocol Marker<T>
          fn mark(self) -> T
        end

        struct Pair<T, E>
          fallback: T
          source: E
        end

        impl Marker<T> for Pair<T, E: Source<T>>
          fn mark(self) -> T
            self.fallback
          end
        end

        struct IntSource
          value: Int
        end

        impl Source<Int> for IntSource
          fn first(self) -> Int
            self.value
          end
        end

        fn use_marker<X: Marker<String>>(value: X) -> String
          value.mark()
        end

        use_marker(Pair{fallback: \"no\", source: IntSource{value: 1}})
        ";

    assert_script_fails_with(source, &["does not implement protocol `Marker<String>`"]);
}
