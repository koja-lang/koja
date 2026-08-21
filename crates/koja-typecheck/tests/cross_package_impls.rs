//! Cross-package `impl P for T`: any package may conform any type to
//! any protocol. There is no orphan rule.
//!
//! The local-protocol direction carries the codec pattern: a package
//! defines `Encodable`, implements it for stdlib vocabulary types,
//! and bounds like `T: Encodable` see the conformance. The
//! both-foreign direction carries the adapter pattern: a glue
//! package conforms one dependency's type to another dependency's
//! protocol. Methods register under the target type's qualified
//! identifier (like `extend`), so bare dot-call works, and
//! coherence is collision detection: duplicate `(protocol, type)`
//! impls and same-named methods on one type are whole-program
//! errors. The inverse direction (foreign protocol, local type) is
//! pinned by the existing protocol and Process suites.

use koja_ast::identifier::{Resolution, ResolvedType};
use koja_ast::util::dedent;
use koja_parser::ParseMode;

mod common;

use common::{
    PACKAGE, assert_script_fails_with, check_packages, diagnostic_messages, global_id, registry_id,
    typecheck_script,
};

/// Check `files` expecting failure, and assert every needle appears
/// in at least one diagnostic message.
fn assert_packages_fail_with(files: &[(&str, &str, &str)], needles: &[&str]) {
    let failure = check_packages(files, ParseMode::Script)
        .expect_err("expected typecheck to fail (test sources must produce a diagnostic)");
    let messages = diagnostic_messages(&failure);
    for needle in needles {
        assert!(
            messages.iter().any(|m| m.contains(needle)),
            "expected a diagnostic containing `{needle}`, got: {messages:#?}",
        );
    }
}

#[test]
fn local_protocol_for_foreign_builtin_records_conformance() {
    let checked = typecheck_script(&dedent(
        "
        protocol Encodable
          fn to_wire(self) -> String
        end

        impl Encodable for String
          fn to_wire(self) -> String
            self
          end
        end

        \"hi\".print()
        ",
    ));
    let string_id = global_id(&checked, "String");
    let protocol_id = registry_id(&checked, PACKAGE, &["Encodable"]);
    assert!(
        checked
            .registry
            .lookup_conformance(string_id, protocol_id, &[])
            .is_some(),
        "expected `String : Encodable` conformance on the Global.String entry",
    );
}

#[test]
fn bound_discharged_by_cross_package_conformance() {
    typecheck_script(&dedent(
        "
        protocol Encodable
          fn to_wire(self) -> String
        end

        impl Encodable for String
          fn to_wire(self) -> String
            self
          end
        end

        fn render<T: Encodable>(value: T) -> String
          value.to_wire()
        end

        render(\"hi\").print()
        ",
    ));
}

#[test]
fn bare_dot_call_resolves_on_foreign_conformance() {
    typecheck_script(&dedent(
        "
        protocol Encodable
          fn to_wire(self) -> String
        end

        impl Encodable for String
          fn to_wire(self) -> String
            self
          end
        end

        \"hi\".to_wire().print()
        ",
    ));
}

#[test]
fn foreign_protocol_for_foreign_type_conforms() {
    // The adapter pattern: a third package conforms a foreign type
    // to a foreign protocol, and bounds see the conformance.
    let checked = check_packages(
        &[
            (
                "Lib",
                "lib.koja",
                "
                protocol Marked
                  fn marked?(self) -> Bool
                end

                fn any_marked?<T: Marked>(value: T) -> Bool
                  value.marked?()
                end
                ",
            ),
            (
                PACKAGE,
                "main.kojs",
                "
                impl Lib.Marked for String
                  fn marked?(self) -> Bool
                    true
                  end
                end

                Lib.any_marked?(\"hi\").print()
                ",
            ),
        ],
        ParseMode::Script,
    )
    .expect("a foreign protocol on a foreign type should typecheck");
    let string_id = global_id(&checked, "String");
    let protocol_id = registry_id(&checked, "Lib", &["Marked"]);
    assert!(
        checked
            .registry
            .lookup_conformance(string_id, protocol_id, &[])
            .is_some(),
        "expected `String : Lib.Marked` conformance on the Global.String entry",
    );
}

#[test]
fn duplicate_conformance_across_packages_diagnosed() {
    // Only the protocol's package and the target's package may write
    // `impl Marked for Point`. When both do, the whole-program
    // conformance table catches the collision.
    assert_packages_fail_with(
        &[
            (
                "LibA",
                "liba.koja",
                "
                protocol Marked
                  fn tag(self) -> String
                end

                impl Marked for LibB.Point
                  fn tag(self) -> String
                    \"from LibA\"
                  end
                end
                ",
            ),
            (
                "LibB",
                "libb.koja",
                "
                struct Point
                  x: Int
                end

                impl LibA.Marked for Point
                  fn tag(self) -> String
                    \"from LibB\"
                  end
                end
                ",
            ),
            (PACKAGE, "main.kojs", "1.print()"),
        ],
        &["already defined", "duplicate `impl"],
    );
}

#[test]
fn foreign_generic_concrete_instantiation_conforms() {
    // `impl Encodable for List<Int>` records a concrete fact keyed
    // by the instantiation, so `List<Int>` conforms and
    // `List<String>` does not.
    let checked = typecheck_script(&dedent(
        "
        protocol Encodable
          fn to_wire(self) -> String
        end

        impl Encodable for List<Int>
          fn to_wire(self) -> String
            \"list\"
          end
        end

        1.print()
        ",
    ));
    let list_id = global_id(&checked, "List");
    let protocol_id = registry_id(&checked, PACKAGE, &["Encodable"]);
    let int_arg = ResolvedType::leaf(Resolution::Global(global_id(&checked, "Int")));
    let string_arg = ResolvedType::leaf(Resolution::Global(global_id(&checked, "String")));
    assert!(
        checked
            .registry
            .lookup_conformance(list_id, protocol_id, std::slice::from_ref(&int_arg))
            .is_some(),
        "expected `List<Int> : Encodable` to be recorded",
    );
    assert!(
        checked
            .registry
            .lookup_conformance(list_id, protocol_id, std::slice::from_ref(&string_arg))
            .is_none(),
        "expected `List<String>` to stay unconformant",
    );
}

#[test]
fn bound_matches_only_the_conforming_instantiation() {
    typecheck_script(&dedent(
        "
        protocol Encodable
          fn to_wire(self) -> String
        end

        impl Encodable for List<Int>
          fn to_wire(self) -> String
            \"list\"
          end
        end

        fn render<T: Encodable>(value: T) -> String
          value.to_wire()
        end

        render([1, 2]).print()
        ",
    ));
}

#[test]
fn bound_rejects_the_other_instantiation() {
    assert_script_fails_with(
        "
        protocol Encodable
          fn to_wire(self) -> String
        end

        impl Encodable for List<Int>
          fn to_wire(self) -> String
            \"list\"
          end
        end

        fn render<T: Encodable>(value: T) -> String
          value.to_wire()
        end

        render([\"a\"]).print()
        ",
        &["does not implement protocol `Encodable`"],
    );
}

#[test]
fn parameterized_stdlib_conformance_matches_every_instantiation() {
    // `impl Debug for List<T>`-shaped stdlib facts stay
    // parameterized, so any instantiation discharges the bound.
    typecheck_script(&dedent(
        "
        fn show<T: Debug>(value: T) -> String
          value.format()
        end

        show([1, 2]).print()
        show([\"a\"]).print()
        ",
    ));
}

#[test]
fn foreign_parameterized_target_rejected() {
    assert_packages_fail_with(
        &[(
            PACKAGE,
            "main.kojs",
            "
            protocol Encodable
              fn to_wire(self) -> String
            end

            impl Encodable for List<T>
              fn to_wire(self) -> String
                \"list\"
              end
            end

            1.print()
            ",
        )],
        &["parameterized conformance for a type from another package"],
    );
}

#[test]
fn mixed_target_args_rejected() {
    assert_packages_fail_with(
        &[(
            PACKAGE,
            "main.kojs",
            "
            protocol Encodable
              fn to_wire(self) -> String
            end

            impl Encodable for Map<String, V>
              fn to_wire(self) -> String
                \"map\"
              end
            end

            1.print()
            ",
        )],
        &["mix type parameters with concrete types"],
    );
}

#[test]
fn duplicate_concrete_instantiation_diagnosed() {
    assert_packages_fail_with(
        &[(
            PACKAGE,
            "main.kojs",
            "
            protocol Encodable
              fn to_wire(self) -> String
            end

            impl Encodable for List<Int>
              fn to_wire(self) -> String
                \"one\"
              end
            end

            impl Encodable for List<Int>
              fn to_wire(self) -> String
                \"two\"
              end
            end

            1.print()
            ",
        )],
        &["duplicate `impl", "Global.List<Global.Int>`"],
    );
}

#[test]
fn duplicate_concrete_instantiation_across_packages_diagnosed() {
    assert_packages_fail_with(
        &[
            (
                "Lib",
                "lib.koja",
                "
                protocol Encodable
                  fn to_wire(self) -> String
                end

                impl Encodable for List<Int>
                  fn to_wire(self) -> String
                    \"from Lib\"
                  end
                end
                ",
            ),
            (
                PACKAGE,
                "main.kojs",
                "
                impl Lib.Encodable for List<Int>
                  fn to_wire(self) -> String
                    \"from app\"
                  end
                end

                1.print()
                ",
            ),
        ],
        &["duplicate `impl", "Global.List<Global.Int>`"],
    );
}

#[test]
fn distinct_instantiations_coexist_as_facts_but_collide_on_method_names() {
    // Known limitation. `impl Encodable for List<Int>` and
    // `impl Encodable for List<String>` are distinct conformance
    // facts (no duplicate-impl diagnostic), but both register
    // `to_wire` in the flat `Global.List` method namespace, so the
    // pair still fails to compile.
    let failure = check_packages(
        &[(
            PACKAGE,
            "main.kojs",
            "
            protocol Encodable
              fn to_wire(self) -> String
            end

            impl Encodable for List<Int>
              fn to_wire(self) -> String
                \"ints\"
              end
            end

            impl Encodable for List<String>
              fn to_wire(self) -> String
                \"strings\"
              end
            end

            1.print()
            ",
        )],
        ParseMode::Script,
    )
    .expect_err("the shared method name must collide");
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("already defined")),
        "expected a method-name collision, got: {messages:#?}",
    );
    assert!(
        !messages.iter().any(|m| m.contains("duplicate `impl")),
        "distinct instantiations must not read as duplicate conformances, got: {messages:#?}",
    );
}

#[test]
fn foreign_priv_target_rejected() {
    assert_packages_fail_with(
        &[
            (
                "Lib",
                "lib.koja",
                "
                priv struct Hidden
                  x: Int
                end
                ",
            ),
            (
                PACKAGE,
                "main.kojs",
                "
                protocol Encodable
                  fn to_wire(self) -> String
                end

                impl Encodable for Lib.Hidden
                  fn to_wire(self) -> String
                    \"hidden\"
                  end
                end

                1.print()
                ",
            ),
        ],
        &["private struct `Lib.Hidden` cannot be referenced from package `TestApp`"],
    );
}

#[test]
fn same_named_methods_from_two_packages_collide() {
    // Two packages legally implement their own protocols for one
    // foreign type, but both protocols declare `tag`, so the methods
    // land in the same `Global.String.tag` slot.
    assert_packages_fail_with(
        &[
            (
                "LibA",
                "liba.koja",
                "
                protocol Wire
                  fn tag(self) -> String
                end

                impl Wire for String
                  fn tag(self) -> String
                    \"wire\"
                  end
                end
                ",
            ),
            (
                "LibB",
                "libb.koja",
                "
                protocol Label
                  fn tag(self) -> String
                end

                impl Label for String
                  fn tag(self) -> String
                    \"label\"
                  end
                end
                ",
            ),
            (PACKAGE, "main.kojs", "1.print()"),
        ],
        &["already defined"],
    );
}
