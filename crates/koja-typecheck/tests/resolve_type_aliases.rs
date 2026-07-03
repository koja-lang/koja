//! Top-level `type X = T` aliases, the surface companion to the
//! file-level `alias` slice (which lives in `tests/aliases.rs` and
//! covers cross-package re-exports).
//!
//! `type X = T` registers `X` as a [`crate::registry::GlobalKind::TypeAlias`]
//! entry in the current package. Resolution at use sites returns a
//! `ResolvedType::Named { resolution, .. }` that points at the alias
//! identifier so diagnostics keep the user's name. Equivalence
//! peels through the alias when comparing types, so `Pet` ≡
//! `Cat | Dog | Fish` for `types_equivalent` purposes.
//!
//! Pinned shapes:
//!
//! - Alias to a union resolves at a use site (param / return / let
//!   slot) and accepts every member as a widening source.
//! - Alias display name round-trips through diagnostics: a
//!   mismatch reports the alias, not the expanded union.
//! - Aliases are reachable cross-package (the registry promotes
//!   them to package-global entries).
//! - Self-referential aliases (`type X = X | A`) surface a precise
//!   cycle diagnostic.
//! - The same alias is exhaustiveness-aware in `match`: a typed-
//!   binding arm hits each member of the underlying union, missing
//!   members surface a clean diagnostic.

use std::path::PathBuf;

use koja_ast::util::dedent;
use koja_parser::{ParseMode, SourceFile, parse_program};
use koja_typecheck::{CheckFailure, CheckedProgram, check_program};

mod common;

use common::{
    PACKAGE, diagnostic_messages, typecheck_file as typecheck,
    typecheck_file_fail as typecheck_fail,
};

/// Multi-package driver: stack the named packages on top of the
/// the autoimports + qualified slice and run the full typecheck
/// pipeline. Used by [`alias_is_visible_cross_package`] to prove
/// `type` aliases promote to package-global entries (unlike file-
/// private `alias Pkg.Type` lines, which the existing
/// `alias_is_file_private` test in `tests/aliases.rs` pins).
fn check_multi_package(packages: &[(&str, &str, &str)]) -> Result<CheckedProgram, CheckFailure> {
    let mut sources = koja_stdlib::autoimport_sources();
    sources.extend(koja_stdlib::qualified_sources());
    for (package, file, body) in packages {
        sources.push(SourceFile {
            package: package.to_string(),
            path: PathBuf::from(file),
            source: body.to_string(),
        });
    }
    check_program(parse_program(sources, ParseMode::File))
}

#[test]
fn type_alias_to_struct_resolves_at_use_site() {
    let source = "
        struct Cat
          name: String
        end

        type Feline = Cat

        fn rename(c: Feline) -> Feline
          c
        end

        fn main -> Feline
          rename(Cat{name: \"whiskers\"})
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn type_alias_to_union_resolves_at_use_site() {
    let source = "
        struct Cat
          name: String
        end

        struct Dog
          name: String
        end

        struct Fish
          color: String
        end

        type Pet = Cat | Dog | Fish

        fn describe(pet: Pet) -> String
          match pet
            _ -> \"a pet\"
          end
        end

        fn main -> String
          describe(Cat{name: \"whiskers\"})
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn type_alias_widens_member_at_call_site() {
    // Passing a bare `Cat` into a `Pet` slot widens through the
    // alias's underlying union: `check_compatible` peels the
    // alias before checking member-of-union, so the alias is
    // semantically transparent to coercion.
    let source = "
        struct Cat
          name: String
        end

        struct Dog
          name: String
        end

        type Pet = Cat | Dog

        fn name_of(p: Pet) -> String
          match p
            c: Cat -> c.name
            d: Dog -> d.name
          end
        end

        fn main -> String
          name_of(Cat{name: \"whiskers\"})
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn type_alias_preserves_name_in_mismatch_diagnostics() {
    // The mismatch diagnostic reports the alias (`Pet`), not the
    // expanded `Cat | Dog`. This is the headline contract for the
    // user-facing display path: alias users see their alias.
    let source = "
        struct Cat
          name: String
        end

        struct Dog
          name: String
        end

        struct Mouse
          color: String
        end

        type Pet = Cat | Dog

        fn name_of(p: Pet) -> String
          match p
            c: Cat -> c.name
            d: Dog -> d.name
          end
        end

        fn main -> String
          name_of(Mouse{color: \"grey\"})
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("Pet") && !m.contains("Cat | Dog")),
        "expected diagnostic to keep the alias name `Pet` (and not expand to \
         `Cat | Dog`), got {messages:?}",
    );
}

#[test]
fn type_alias_member_typed_binding_arms_resolve() {
    // The match driver peels the alias before computing union
    // exhaustiveness, so typed-binding arms over a `Pet`-typed
    // subject narrow to each underlying member.
    let source = "
        struct Cat
          name: String
        end

        struct Dog
          name: String
        end

        type Pet = Cat | Dog

        fn name_of(p: Pet) -> String
          match p
            c: Cat -> c.name
            d: Dog -> d.name
          end
        end

        fn main -> String
          name_of(Cat{name: \"whiskers\"})
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn type_alias_match_missing_member_diagnoses() {
    // Exhaustiveness names the missing member by its surface type
    // (here `Dog`): alias peeling threads through the missing-
    // member walk, so the user sees a member of the union, not the
    // alias name "Pet".
    let source = "
        struct Cat
          name: String
        end

        struct Dog
          name: String
        end

        type Pet = Cat | Dog

        fn name_of(p: Pet) -> String
          match p
            c: Cat -> c.name
          end
        end

        fn main -> String
          name_of(Cat{name: \"whiskers\"})
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("Dog") && m.to_lowercase().contains("missing")),
        "expected missing-member diagnostic mentioning `Dog`, got {messages:?}",
    );
}

#[test]
fn self_referential_type_alias_diagnoses_cycle() {
    let source = "
        struct A
          x: Int
        end

        type Loop = Loop | A

        fn main -> Int
          0
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.to_lowercase().contains("cycle") || m.to_lowercase().contains("recursive")),
        "expected alias-cycle diagnostic, got {messages:?}",
    );
}

#[test]
fn type_alias_is_visible_cross_package() {
    // `pets.koja` declares `type Pet = Cat | Dog`. `app.koja` in a
    // sibling package brings `Pets.Pet` and `Pets.Cat` into scope
    // via `alias` and uses the alias as a type. The registry
    // promotes `type` aliases to package-global entries so the
    // alias machinery finds them just like it finds structs and
    // enums. Without that promotion the `alias Pets.Pet` line
    // would diagnose with "alias target is not a registered type."
    let result = check_multi_package(&[
        (
            "Pets",
            "pets.koja",
            "struct Cat\n  name: String\nend\n\
             struct Dog\n  name: String\nend\n\
             type Pet = Cat | Dog\n",
        ),
        (
            PACKAGE,
            "app.koja",
            "alias Pets.Cat\n\
             alias Pets.Pet\n\
             fn label(p: Pet) -> String\n  \
                match p\n    \
                  _ -> \"pet\"\n  \
                end\n\
              end\n\
              fn main -> String\n  \
                label(Cat{name: \"whiskers\"})\n\
              end\n",
        ),
    ]);
    assert!(
        result.is_ok(),
        "expected cross-package alias to resolve, got {:?}",
        result.err().map(|f| diagnostic_messages(&f)),
    );
}
