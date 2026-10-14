//! Typecheck coverage for inference from a protocol bound. A type
//! parameter that appears only as an argument of another parameter's
//! bound, `E` in `fn drain<R: Reader<E>, E>(source: R)`, has no
//! argument to bind from. `fill_from_bounds` in
//! `pipeline::resolve::inference` reads the bounded type's
//! conformance and fills it from there. `bounds.rs` covers a bound
//! being remembered and enforced. This file covers inference from
//! one.

use koja_ast::identifier::ResolvedType;
use koja_ast::util::dedent;
use koja_typecheck::CheckedProgram;

mod common;

use common::{
    assert_script_fails_with, diagnostic_messages, global_named, int_type, string_type,
    trailing_resolution, typecheck_script as typecheck, typecheck_script_fail as typecheck_fail,
};

/// A protocol with one argument, one conforming type, and a generic
/// consumer whose error parameter appears only in the bound.
const READER_PRELUDE: &str = "
    protocol Reader<E>
      fn read(self) -> Int ! E
    end

    struct Source
    end

    impl Reader<String> for Source
      fn read(self) -> Int ! String
        1
      end
    end

    fn drain<R: Reader<E>, E>(source: R) -> Int ! E
      try source.read()
    end
    ";

fn reader_script(extra: &str) -> String {
    format!("{}\n{}\n", dedent(READER_PRELUDE), dedent(extra))
}

fn result_of_int_and_string(checked: &CheckedProgram) -> ResolvedType {
    global_named(
        checked,
        "Result",
        vec![int_type(checked), string_type(checked)],
    )
}

#[test]
fn bound_argument_fills_from_the_conformance() {
    let checked = typecheck(&reader_script("drain(Source{})"));
    assert_eq!(
        trailing_resolution(&checked),
        result_of_int_and_string(&checked),
    );
}

#[test]
fn one_bound_fills_two_slots() {
    // `List<Int>` conforms with `Enumeration<Int, Int>`, so one
    // conformance answers both `T` and `Cursor`.
    let source = "
        fn count_items<T, Cursor, E: Enumeration<T, Cursor>>(source: E) -> Int
          count = 0

          for _ in source
            count += 1
          end

          count
        end

        count_items([1, 2, 3])
        ";

    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn type_param_head_fills_from_its_granted_bound() {
    // Inside `outer`, `s` is a type param. Its declared bound names
    // the protocol argument, so `drain(s)` fills `E` from it.
    let extra = "
        fn outer<S: Reader<String>>(s: S) -> Int ! String
          try drain(s)
        end

        outer(Source{})
        ";

    let checked = typecheck(&reader_script(extra));
    assert_eq!(
        trailing_resolution(&checked),
        result_of_int_and_string(&checked),
    );
}

#[test]
fn argument_binding_wins_over_the_conformance() {
    let extra = "
        fn drain2<R: Reader<E>, E>(source: R, witness: E) -> Int ! E
          try source.read()
        end

        drain2(Source{}, 1)
        ";

    assert_script_fails_with(
        &reader_script(extra),
        &[
            "does not implement protocol `TestApp.Reader<Int>`",
            "required by type parameter `R`",
        ],
    );
}

#[test]
fn missing_conformance_reports_the_slot_once() {
    let extra = "
        struct Plain
        end

        drain(Plain{})
        ";

    let failure = typecheck_fail(&reader_script(extra));
    let messages = diagnostic_messages(&failure);
    assert_eq!(messages.len(), 1, "got: {messages:#?}");
    assert!(
        messages[0].contains("cannot infer type parameter `E`"),
        "unexpected diagnostic: {}",
        messages[0],
    );
    assert!(
        !messages[0].contains("<unresolved>"),
        "diagnostic leaks an unresolved slot: {}",
        messages[0],
    );
}
