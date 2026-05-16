//! `priv fn` visibility enforcement at call sites.
//!
//! Pins the contract surface for the visibility check threaded
//! through [`expo_typecheck::pipeline::resolve::calls`]: every
//! `priv fn` carries a [`VisibilityScope`] in the registry (top-level
//! `→` `PackagePrivate`, method `→` `TypePrivate(owner)`), and bare
//! / method call resolution rejects callers that fall outside that
//! scope. Surface syntax can't currently dispatch `Pkg.fn(args)` to
//! a top-level function in another package, so the cross-package
//! rejection path is covered by the unit test on `callee_is_visible`
//! rather than by an integration case here.
//!
//! [`VisibilityScope`]: expo_typecheck::registry::VisibilityScope

use std::path::PathBuf;

use expo_ast::util::dedent;
use expo_parser::{ParseMode, SourceFile, parse_program};
use expo_typecheck::{CheckFailure, CheckedProgram, check_program};

mod common;

use common::{PACKAGE, diagnostic_messages, typecheck_file, typecheck_file_fail};

/// Drive `parse_program → check_program` on multiple user files
/// stacked in `TestApp`. Used by
/// [`top_level_priv_callable_across_files_in_same_package`] to prove
/// `priv fn` reaches sibling files inside one package.
fn check_multi_file(files: &[(&str, &str)]) -> Result<CheckedProgram, CheckFailure> {
    let mut sources = expo_stdlib::autoimport_sources();
    sources.extend(expo_stdlib::qualified_sources());
    for (name, body) in files {
        sources.push(SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from(name),
            source: dedent(body),
        });
    }
    check_program(parse_program(sources, ParseMode::File))
}

#[test]
fn top_level_priv_callable_from_same_file() {
    typecheck_file(&dedent(
        "
        priv fn double(x: Int) -> Int
          x * 2
        end

        fn main
          double(21).print()
        end
        ",
    ));
}

#[test]
fn top_level_priv_callable_across_files_in_same_package() {
    let result = check_multi_file(&[
        (
            "helper.expo",
            "
            priv fn double(x: Int) -> Int
              x * 2
            end
            ",
        ),
        (
            "main.expo",
            "
            fn main
              double(21).print()
            end
            ",
        ),
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
    typecheck_file(&dedent(
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

        fn main
          Counter { value: 21 }.boosted().print()
        end
        ",
    ));
}

#[test]
fn priv_method_callable_from_sibling_method_in_decl_block() {
    typecheck_file(&dedent(
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

        fn main
          Counter { value: 1 }.next().print()
        end
        ",
    ));
}

#[test]
fn priv_method_callable_across_impl_blocks_on_same_type() {
    // `priv fn helper` is declared inside the struct decl, but the
    // calling method lives in a separate `impl Counter` block. Both
    // register under `TestApp.Counter`, so the visibility scope
    // covers both blocks.
    typecheck_file(&dedent(
        "
        struct Counter
          value: Int

          priv fn helper(self) -> Int
            self.value * 2
          end
        end

        impl Counter
          fn doubled(self) -> Int
            self.helper()
          end
        end

        fn main
          Counter { value: 7 }.doubled().print()
        end
        ",
    ));
}

#[test]
fn priv_method_rejected_from_another_type() {
    let failure = typecheck_file_fail(&dedent(
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

        fn main
          Probe {}.poke().print()
        end
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
fn priv_method_rejected_from_top_level_function() {
    let failure = typecheck_file_fail(&dedent(
        "
        struct Counter
          value: Int

          priv fn helper(self) -> Int
            self.value * 2
          end
        end

        fn main
          Counter { value: 1 }.helper().print()
        end
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
