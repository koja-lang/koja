//! `priv` visibility enforcement at reference sites.
//!
//! Two enforcement seams are pinned here. At call sites, every `priv fn`
//! carries a [`VisibilityScope`] in the registry (top-level `->`
//! `PackagePrivate`, method `->` `TypePrivate(owner)`), and bare /
//! method call resolution rejects callers that fall outside that
//! scope. At reference sites, `priv` structs, enums, constants, type
//! aliases, and protocols are `PackagePrivate`, and every reference
//! position (signature type expressions, constructors, patterns,
//! static receivers, `extend` targets, `alias` targets) rejects
//! other packages while staying usable across files of the declaring
//! package. The signature leak check (public surface exposing a
//! same-package private type) is pinned at the end.
//!
//! Everything runs in script mode so use-site statements sit at top
//! level without `fn main` wrappers. The enforcement seams live in
//! collect / lift / resolve, which are mode-agnostic.
//!
//! [`VisibilityScope`]: koja_typecheck::registry::VisibilityScope

use std::path::PathBuf;

use koja_ast::util::dedent;
use koja_parser::{ParseMode, SourceFile, parse_program};
use koja_typecheck::{CheckFailure, CheckedProgram, check_program};

mod common;

use common::{PACKAGE, diagnostic_messages, typecheck_script, typecheck_script_fail};

/// Drive `parse_program -> check_program` on multiple user files
/// stacked in `TestApp`. Used by the same-package positives to prove
/// `priv` decls reach sibling files inside one package.
fn check_multi_file(files: &[(&str, &str)]) -> Result<CheckedProgram, CheckFailure> {
    let stacked: Vec<(&str, &str, &str)> = files
        .iter()
        .map(|(name, body)| (PACKAGE, *name, *body))
        .collect();
    check_packages(&stacked)
}

/// Drive `parse_program -> check_program` on `(package, filename,
/// body)` triples so cross-package rejection cases can stack a `Lib`
/// package next to `TestApp`.
fn check_packages(files: &[(&str, &str, &str)]) -> Result<CheckedProgram, CheckFailure> {
    let mut sources = koja_stdlib::autoimport_sources();
    sources.extend(koja_stdlib::qualified_sources());
    for (package, name, body) in files {
        sources.push(SourceFile {
            package: package.to_string(),
            path: PathBuf::from(name),
            source: dedent(body),
        });
    }
    check_program(parse_program(sources, ParseMode::Script))
}

/// Assert the failure contains the standard cross-package rejection
/// message for `kind_label` (e.g. "struct") on `identifier`.
fn assert_private_reference_rejected(failure: &CheckFailure, kind_label: &str, identifier: &str) {
    let needle = format!(
        "private {kind_label} `{identifier}` cannot be referenced from package `{PACKAGE}`"
    );
    let messages = diagnostic_messages(failure);
    assert!(
        messages.iter().any(|m| m.contains(&needle)),
        "expected `{needle}`, got {messages:?}",
    );
}

#[test]
fn top_level_priv_callable_from_same_file() {
    typecheck_script(&dedent(
        "
        priv fn double(x: Int) -> Int
          x * 2
        end

        double(21).print()
        ",
    ));
}

#[test]
fn top_level_priv_callable_across_files_in_same_package() {
    let result = check_multi_file(&[
        (
            "helper.koja",
            "
            priv fn double(x: Int) -> Int
              x * 2
            end
            ",
        ),
        ("main.kojs", "double(21).print()"),
    ])
    .expect("same-package cross-file priv call should succeed");
    assert!(
        result.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        result.diagnostics,
    );
}

#[test]
fn top_level_priv_callable_from_method_in_same_package() {
    typecheck_script(&dedent(
        "
        priv fn double(x: Int) -> Int
          x * 2
        end

        struct Counter
          value: Int

          fn boosted(self) -> Int
            double(self.value)
          end
        end

        Counter { value: 21 }.boosted().print()
        ",
    ));
}

#[test]
fn priv_method_callable_from_sibling_method_in_decl_block() {
    typecheck_script(&dedent(
        "
        struct Counter
          value: Int

          fn next(self) -> Int
            self.tick() + 1
          end

          priv fn tick(self) -> Int
            self.value + 1
          end
        end

        Counter { value: 1 }.next().print()
        ",
    ));
}

#[test]
fn priv_method_callable_across_impl_blocks_on_same_type() {
    // `priv fn helper` is declared inside the struct decl, but the
    // calling method lives in a separate `impl Counter` block. Both
    // register under `TestApp.Counter`, so the visibility scope
    // covers both blocks.
    typecheck_script(&dedent(
        "
        struct Counter
          value: Int

          priv fn helper(self) -> Int
            self.value * 2
          end
        end

        extend Counter
          fn doubled(self) -> Int
            self.helper()
          end
        end

        Counter { value: 7 }.doubled().print()
        ",
    ));
}

#[test]
fn priv_method_rejected_from_another_type() {
    let failure = typecheck_script_fail(&dedent(
        "
        struct Counter
          value: Int

          priv fn helper(self) -> Int
            self.value * 2
          end
        end

        struct Probe
          fn poke -> Int
            Counter { value: 1 }.helper()
          end
        end

        Probe {}.poke().print()
        ",
    ));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("private method `TestApp.Counter.helper`")
                && m.contains("cannot be called from here")),
        "expected type-private rejection diagnostic, got {messages:?}",
    );
}

#[test]
fn priv_method_rejected_from_top_level_code() {
    let failure = typecheck_script_fail(&dedent(
        "
        struct Counter
          value: Int

          priv fn helper(self) -> Int
            self.value * 2
          end
        end

        Counter { value: 1 }.helper().print()
        ",
    ));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("private method `TestApp.Counter.helper`")
                && m.contains("cannot be called from here")),
        "expected type-private rejection diagnostic, got {messages:?}",
    );
}

// ---------------------------------------------------------------------------
// Same-package positives: `priv` decls reach sibling files
// ---------------------------------------------------------------------------

#[test]
fn priv_struct_usable_across_files_in_same_package() {
    check_multi_file(&[
        (
            "lib.koja",
            "
            priv struct Counter
              value: Int
            end

            priv fn bump(c: Counter) -> Counter
              Counter{value: c.value + 1}
            end
            ",
        ),
        (
            "main.kojs",
            "
            c = bump(Counter{value: 1})
            c.value.print()
            ",
        ),
    ])
    .expect("same-package priv struct use should succeed");
}

#[test]
fn priv_enum_usable_across_files_in_same_package() {
    check_multi_file(&[
        (
            "lib.koja",
            "
            priv enum Mode
              Off
              On
            end
            ",
        ),
        (
            "main.kojs",
            "
            m = Mode.On
            match m
              Mode.On -> \"on\".print()
              Mode.Off -> \"off\".print()
            end
            ",
        ),
    ])
    .expect("same-package priv enum use should succeed");
}

#[test]
fn priv_constant_usable_across_files_in_same_package() {
    check_multi_file(&[
        ("lib.koja", "priv const LIMIT: Int = 10"),
        ("main.kojs", "LIMIT.print()"),
    ])
    .expect("same-package priv constant use should succeed");
}

#[test]
fn priv_type_alias_usable_across_files_in_same_package() {
    check_multi_file(&[
        (
            "lib.koja",
            "
            priv struct Cat
              name: String
            end

            priv struct Dog
              name: String
            end

            priv type Pet = Cat | Dog
            ",
        ),
        (
            "main.kojs",
            "
            priv fn name_of(p: Pet) -> String
              match p
                c: Cat -> c.name
                d: Dog -> d.name
              end
            end

            name_of(Cat{name: \"whiskers\"}).print()
            ",
        ),
    ])
    .expect("same-package priv type alias use should succeed");
}

#[test]
fn priv_protocol_implementable_in_same_package() {
    check_multi_file(&[
        (
            "lib.koja",
            "
            priv protocol Marked
              fn mark(self) -> Int
            end
            ",
        ),
        (
            "main.kojs",
            "
            struct Point
              x: Int
            end

            impl Marked for Point
              fn mark(self) -> Int
                self.x
              end
            end

            Point{x: 3}.mark().print()
            ",
        ),
    ])
    .expect("same-package priv protocol impl should succeed");
}

// ---------------------------------------------------------------------------
// Cross-package negatives: `priv` decls reject other packages
// ---------------------------------------------------------------------------

/// A `Lib` package exporting one private decl per kind, plus a
/// public control struct.
const LIB_DECLS: &str = "
    priv struct Hidden
      value: Int

      fn make -> Hidden
        Hidden{value: 1}
      end
    end

    priv enum Mode
      Off
      On
    end

    priv type Secret = Int

    priv protocol Marked
      fn mark(self) -> Int
    end

    struct Open
      value: Int
    end
    ";

fn check_lib_and_app(app: &str) -> Result<CheckedProgram, CheckFailure> {
    check_packages(&[("Lib", "lib.koja", LIB_DECLS), (PACKAGE, "main.kojs", app)])
}

#[test]
fn public_struct_usable_cross_package_control() {
    check_lib_and_app("Lib.Open{value: 1}.value.print()")
        .expect("public cross-package struct use should succeed");
}

#[test]
fn priv_struct_construction_rejected_cross_package() {
    let failure = check_lib_and_app("Lib.Hidden{value: 1}.value.print()")
        .expect_err("cross-package priv struct construction should fail");
    assert_private_reference_rejected(&failure, "struct", "Lib.Hidden");
}

#[test]
fn priv_struct_in_signature_rejected_cross_package() {
    let failure = check_lib_and_app(
        "
        fn probe(h: Lib.Hidden) -> Int
          h.value
        end
        ",
    )
    .expect_err("cross-package priv struct in signature should fail");
    assert_private_reference_rejected(&failure, "struct", "Lib.Hidden");
}

#[test]
fn priv_enum_construction_rejected_cross_package() {
    let failure = check_lib_and_app(
        "
        m = Lib.Mode.On
        m.print()
        ",
    )
    .expect_err("cross-package priv enum construction should fail");
    assert_private_reference_rejected(&failure, "enum", "Lib.Mode");
}

#[test]
fn priv_static_receiver_rejected_cross_package() {
    let failure = check_lib_and_app("Lib.Hidden.make().value.print()")
        .expect_err("cross-package static call on priv type should fail");
    assert_private_reference_rejected(&failure, "struct", "Lib.Hidden");
}

#[test]
fn priv_type_alias_rejected_cross_package() {
    let failure = check_lib_and_app(
        "
        fn probe(s: Lib.Secret) -> Int
          0
        end
        ",
    )
    .expect_err("cross-package priv type alias reference should fail");
    assert_private_reference_rejected(&failure, "type alias", "Lib.Secret");
}

#[test]
fn priv_protocol_impl_rejected_cross_package() {
    let failure = check_lib_and_app(
        "
        struct Point
          x: Int
        end

        impl Lib.Marked for Point
          fn mark(self) -> Int
            self.x
          end
        end
        ",
    )
    .expect_err("cross-package impl of priv protocol should fail");
    assert_private_reference_rejected(&failure, "protocol", "Lib.Marked");
}

#[test]
fn priv_alias_target_rejected_cross_package() {
    let failure = check_lib_and_app("alias Lib.Hidden")
        .expect_err("cross-package alias of priv type should fail");
    assert_private_reference_rejected(&failure, "struct", "Lib.Hidden");
}

#[test]
fn priv_extend_target_rejected_cross_package() {
    let failure = check_lib_and_app(
        "
        extend Lib.Hidden
          fn poke(self) -> Int
            self.value
          end
        end
        ",
    )
    .expect_err("cross-package extend of priv type should fail");
    assert_private_reference_rejected(&failure, "struct", "Lib.Hidden");
}

#[test]
fn priv_struct_pattern_rejected_cross_package() {
    // A well-typed subject of a private type is unconstructable from
    // outside, so the subject here is deliberately mismatched. The
    // pattern path still resolves `Lib.Hidden` and the reference
    // gate fires alongside the subject-mismatch diagnostic.
    let failure = check_lib_and_app(
        "
        fn probe(x: Int) -> Int
          match x
            Lib.Hidden{value: v} -> v
          end
        end
        ",
    )
    .expect_err("cross-package priv struct pattern should fail");
    assert_private_reference_rejected(&failure, "struct", "Lib.Hidden");
}

// ---------------------------------------------------------------------------
// Signature leak check: public surface may not expose same-package privates
// ---------------------------------------------------------------------------

/// Assert the failure contains the leak message for a public
/// `subject` exposing private `leaked` (both "kind `identifier`").
fn assert_leak_rejected(failure: &CheckFailure, subject: &str, leaked: &str) {
    let needle = format!("public {subject} exposes private {leaked}");
    let messages = diagnostic_messages(failure);
    assert!(
        messages.iter().any(|m| m.contains(&needle)),
        "expected `{needle}`, got {messages:?}",
    );
}

#[test]
fn public_fn_param_of_priv_struct_leaks() {
    let failure = typecheck_script_fail(&dedent(
        "
        priv struct Hidden
          value: Int
        end

        fn probe(h: Hidden) -> Int
          h.value
        end
        ",
    ));
    assert_leak_rejected(
        &failure,
        "function `TestApp.probe`",
        "struct `TestApp.Hidden`",
    );
}

#[test]
fn public_fn_return_of_priv_struct_leaks() {
    let failure = typecheck_script_fail(&dedent(
        "
        priv struct Hidden
          value: Int
        end

        fn make -> Hidden
          Hidden{value: 1}
        end
        ",
    ));
    assert_leak_rejected(
        &failure,
        "function `TestApp.make`",
        "struct `TestApp.Hidden`",
    );
}

#[test]
fn public_fn_generic_arg_of_priv_struct_leaks() {
    let failure = typecheck_script_fail(&dedent(
        "
        priv struct Hidden
          value: Int
        end

        fn find -> Option<Hidden>
          Option.None
        end
        ",
    ));
    assert_leak_rejected(
        &failure,
        "function `TestApp.find`",
        "struct `TestApp.Hidden`",
    );
}

#[test]
fn public_struct_field_of_priv_enum_leaks() {
    let failure = typecheck_script_fail(&dedent(
        "
        priv enum Mode
          Off
          On
        end

        struct Config
          mode: Mode
        end
        ",
    ));
    assert_leak_rejected(&failure, "struct `TestApp.Config`", "enum `TestApp.Mode`");
}

#[test]
fn public_enum_payload_of_priv_struct_leaks() {
    let failure = typecheck_script_fail(&dedent(
        "
        priv struct Hidden
          value: Int
        end

        enum Wrapper
          Empty
          Full(Hidden)
        end
        ",
    ));
    assert_leak_rejected(
        &failure,
        "enum `TestApp.Wrapper`",
        "struct `TestApp.Hidden`",
    );
}

#[test]
fn public_type_alias_of_priv_struct_leaks() {
    let failure = typecheck_script_fail(&dedent(
        "
        priv struct Cat
          name: String
        end

        priv struct Dog
          name: String
        end

        type Pet = Cat | Dog
        ",
    ));
    assert_leak_rejected(&failure, "type alias `TestApp.Pet`", "struct `TestApp.Cat`");
    assert_leak_rejected(&failure, "type alias `TestApp.Pet`", "struct `TestApp.Dog`");
}

#[test]
fn public_protocol_method_of_priv_struct_leaks() {
    let failure = typecheck_script_fail(&dedent(
        "
        priv struct Hidden
          value: Int
        end

        protocol Producer
          fn produce(self) -> Hidden
        end
        ",
    ));
    assert_leak_rejected(
        &failure,
        "protocol `TestApp.Producer`",
        "struct `TestApp.Hidden`",
    );
}

#[test]
fn public_constant_of_priv_struct_leaks() {
    let failure = typecheck_script_fail(&dedent(
        "
        priv struct Hidden
          value: Int
        end

        const DEFAULT = Hidden{value: 1}
        ",
    ));
    assert_leak_rejected(
        &failure,
        "constant `TestApp.DEFAULT`",
        "struct `TestApp.Hidden`",
    );
}

#[test]
fn public_fn_bound_on_priv_protocol_leaks() {
    let failure = typecheck_script_fail(&dedent(
        "
        priv protocol Marked
          fn mark(self) -> Int
        end

        fn tally<T: Marked>(x: T) -> Int
          x.mark()
        end
        ",
    ));
    assert_leak_rejected(
        &failure,
        "function `TestApp.tally`",
        "protocol `TestApp.Marked`",
    );
}

#[test]
fn priv_fn_may_mention_priv_type() {
    typecheck_script(&dedent(
        "
        priv struct Hidden
          value: Int
        end

        priv fn bump(h: Hidden) -> Hidden
          Hidden{value: h.value + 1}
        end

        bump(Hidden{value: 1}).value.print()
        ",
    ));
}

#[test]
fn public_method_on_priv_type_may_mention_it() {
    // The method is only nameable through the private type, so its
    // signature mentioning that type leaks nothing. This exemption
    // also keeps derived impls (Debug's `format`) legal on privates.
    typecheck_script(&dedent(
        "
        priv struct Hidden
          value: Int

          fn make -> Hidden
            Hidden{value: 1}
          end

          fn doubled(self) -> Hidden
            Hidden{value: self.value * 2}
          end
        end

        Hidden.make().doubled().value.print()
        ",
    ));
}
