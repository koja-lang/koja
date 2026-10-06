//! `@experimental` marks a declaration as unstable. The message is
//! optional, and every use that resolves to the marked entry warns at
//! the use site. Uses inside the tagged decl itself and inside `impl`
//! / `extend` blocks on a tagged target are suppressed. A package that
//! opts in through `CheckOptions::experimental_packages` is silent.

use std::collections::BTreeSet;
use std::path::Path;

use koja_ast::ast::Severity;
use koja_ast::util::dedent;
use koja_parser::ParseMode;
use koja_typecheck::{CheckOptions, ExperimentalTag};

mod common;

use common::{
    PACKAGE, assert_script_fails_with, check_packages, check_packages_with, registry_id,
    typecheck_file, typecheck_script, typecheck_script_fail, warning_messages,
};

const HINT: &str = "set `experimental = true` under `[project]` in koja.toml to accept this";

/// Warnings from a script-mode source (dedented).
fn script_warnings(source: &str) -> Vec<String> {
    warning_messages(&typecheck_script(&dedent(source)))
}

/// Assert every needle appears in at least one warning.
fn assert_warns(warnings: &[String], needles: &[&str]) {
    for needle in needles {
        assert!(
            warnings.iter().any(|w| w.contains(needle)),
            "expected a warning containing `{needle}`, got: {warnings:#?}",
        );
    }
}

/// The `Lib` declares an experimental struct and `TestApp` uses it.
const CROSS_PACKAGE: &[(&str, &str, &str)] = &[
    (
        "Lib",
        "lib.koja",
        "
        @experimental
        struct Span
          id: Int
        end
        ",
    ),
    (
        PACKAGE,
        "app.koja",
        "
        fn read(s: Lib.Span) -> Int
          s.id
        end
        ",
    ),
];

// Placement and payload validation

#[test]
fn bare_experimental_is_accepted() {
    let warnings = script_warnings(
        "
        @experimental
        fn trace() -> Int
          1
        end
        ",
    );
    assert!(
        warnings.is_empty(),
        "unused decl must not warn: {warnings:?}"
    );
}

#[test]
fn experimental_with_message_is_accepted() {
    let warnings = script_warnings(
        "
        @experimental \"The shape is not final.\"
        fn trace() -> Int
          1
        end
        ",
    );
    assert!(
        warnings.is_empty(),
        "unused decl must not warn: {warnings:?}"
    );
}

#[test]
fn experimental_false_is_rejected() {
    let source = "
        @experimental false
        struct Span
          id: Int
        end
        ";
    assert_script_fails_with(source, &["`@experimental` takes no value or a message"]);
}

#[test]
fn experimental_empty_message_is_rejected() {
    let source = "
        @experimental \"  \"
        fn trace() -> Int
          1
        end
        ";
    assert_script_fails_with(source, &["`@experimental` takes no value or a message"]);
}

#[test]
fn experimental_with_doc_false_is_rejected() {
    let source = "
        @doc false
        @experimental
        fn trace() -> Int
          1
        end
        ";
    assert_script_fails_with(source, &["`@experimental` is not valid with `@doc false`"]);
}

#[test]
fn experimental_on_protocol_method_is_rejected() {
    let source = "
        protocol Show
          @experimental
          fn show(self) -> String
        end
        ";
    assert_script_fails_with(
        source,
        &["annotations on protocol methods", "@experimental"],
    );
}

#[test]
fn experimental_on_priv_decl_is_accepted() {
    let warnings = script_warnings(
        "
        @experimental
        priv struct Hidden
          slot: Int
        end
        ",
    );
    assert!(
        warnings.is_empty(),
        "unused decl must not warn: {warnings:?}"
    );
}

#[test]
fn experimental_is_accepted_on_every_decl_kind() {
    let source = "
        @experimental
        struct Span
          id: Int
        end

        @experimental \"Variants may change.\"
        enum Kind
          Internal
          Server
        end

        @experimental
        protocol Export
          fn export(self) -> String
        end

        @experimental
        const LIMIT: Int = 10

        @experimental
        type Handle = Span

        @experimental
        fn trace() -> Int
          1
        end
        ";
    typecheck_file(&dedent(source));
}

#[test]
fn tag_and_message_are_stamped_on_the_registry_entry() {
    let source = "
        @experimental
        fn bare() -> Int
          1
        end

        @experimental \"The shape is not final.\"
        fn described() -> Int
          2
        end
        ";
    let checked = typecheck_file(&dedent(source));
    let bare = registry_id(&checked, PACKAGE, &["bare"]);
    let described = registry_id(&checked, PACKAGE, &["described"]);
    assert_eq!(
        checked.registry.get(bare).unwrap().experimental,
        Some(ExperimentalTag { message: None })
    );
    assert_eq!(
        checked.registry.get(described).unwrap().experimental,
        Some(ExperimentalTag {
            message: Some("The shape is not final.".to_string()),
        })
    );
}

#[test]
fn multiline_message_is_trimmed() {
    let warnings = script_warnings(
        "
        @experimental \"\"\"
        The shape is not final.
        \"\"\"
        fn trace() -> Int
          1
        end

        trace()
        ",
    );
    assert_warns(
        &warnings,
        &["`trace` is experimental and may change in a later release. The shape is not final."],
    );
}

// Use-site warnings

#[test]
fn call_to_bare_experimental_function_warns_without_a_trailing_message() {
    let checked = typecheck_script(&dedent(
        "
        @experimental
        fn trace() -> Int
          1
        end

        trace()
        ",
    ));
    let warning = checked
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Warning)
        .expect("expected an experimental warning");
    assert_eq!(
        warning.message,
        "`trace` is experimental and may change in a later release."
    );
    assert_eq!(warning.hint.as_deref(), Some(HINT));
}

#[test]
fn call_to_experimental_function_appends_the_message() {
    let warnings = script_warnings(
        "
        @experimental \"The shape is not final.\"
        fn trace() -> Int
          1
        end

        trace()
        ",
    );
    assert_warns(
        &warnings,
        &["`trace` is experimental and may change in a later release. The shape is not final."],
    );
}

#[test]
fn experimental_type_in_signature_position_warns() {
    let warnings = script_warnings(
        "
        @experimental
        struct Span
          id: Int
        end

        fn read(s: Span) -> Int
          s.id
        end
        ",
    );
    assert_warns(&warnings, &["`Span` is experimental"]);
}

#[test]
fn construction_of_experimental_struct_warns() {
    let warnings = script_warnings(
        "
        @experimental
        struct Span
          id: Int
        end

        s = Span{id: 1}
        s.id
        ",
    );
    assert_warns(&warnings, &["`Span` is experimental"]);
}

#[test]
fn experimental_enum_construction_and_pattern_warn() {
    let warnings = script_warnings(
        "
        @experimental
        enum Kind
          Internal
          Server
        end

        k = Kind.Internal
        match k
          Kind.Internal -> 0
          Kind.Server -> 1
        end
        ",
    );
    let hits = warnings
        .iter()
        .filter(|w| w.contains("`Kind` is experimental"))
        .count();
    assert!(
        hits >= 3,
        "expected construction + two pattern warnings, got: {warnings:#?}",
    );
}

#[test]
fn experimental_constant_read_warns() {
    let warnings = script_warnings(
        "
        @experimental
        const LIMIT: Int = 10

        LIMIT + 1
        ",
    );
    assert_warns(&warnings, &["`LIMIT` is experimental"]);
}

#[test]
fn static_call_on_experimental_type_warns() {
    let warnings = script_warnings(
        "
        @experimental
        struct Span
          id: Int

          fn root() -> Span
            Span{id: 0}
          end
        end

        Span.root()
        ",
    );
    assert_warns(&warnings, &["`Span` is experimental"]);
}

#[test]
fn call_to_experimental_method_warns() {
    let warnings = script_warnings(
        "
        struct Point
          x: Int

          @experimental
          fn shift(self) -> Int
            self.x
          end
        end

        p = Point{x: 1}
        p.shift()
        ",
    );
    assert_warns(&warnings, &["`Point.shift` is experimental"]);
}

#[test]
fn experimental_type_alias_use_warns() {
    let warnings = script_warnings(
        "
        struct Cat
          name: String
        end

        @experimental
        type Pet = Cat

        fn feed(pet: Pet) -> Pet
          pet
        end
        ",
    );
    assert_warns(&warnings, &["`Pet` is experimental"]);
}

#[test]
fn experimental_protocol_bound_warns() {
    let warnings = script_warnings(
        "
        @experimental
        protocol Show
          fn show(self) -> String
        end

        fn describe<T: Show>(value: T) -> String
          value.show()
        end
        ",
    );
    assert_warns(&warnings, &["`Show` is experimental"]);
}

#[test]
fn function_reference_to_experimental_function_warns() {
    let warnings = script_warnings(
        "
        @experimental
        fn trace() -> Int
          1
        end

        f = &trace/0
        f()
        ",
    );
    assert_warns(&warnings, &["`trace` is experimental"]);
}

#[test]
fn default_parameter_adapters_warn() {
    let warnings = script_warnings(
        "
        @experimental
        fn trace(depth: Int = 1) -> Int
          depth
        end

        trace()
        ",
    );
    assert_warns(&warnings, &["`trace` is experimental"]);
}

#[test]
fn both_tags_fire_both_warnings() {
    let warnings = script_warnings(
        "
        @deprecated \"Use trace2 instead.\"
        @experimental
        fn trace() -> Int
          1
        end

        trace()
        ",
    );
    assert_warns(
        &warnings,
        &[
            "`trace` is deprecated. Use trace2 instead.",
            "`trace` is experimental and may change in a later release.",
        ],
    );
}

#[test]
fn cross_package_use_warns_in_the_using_file() {
    let checked =
        check_packages(CROSS_PACKAGE, ParseMode::File).expect("cross-package fixture typechecks");
    assert_warns(&warning_messages(&checked), &["`Span` is experimental"]);
    let warning = checked
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Warning)
        .expect("expected an experimental warning");
    assert_eq!(
        checked.path_of(warning.span.file),
        Some(Path::new("app.koja"))
    );
}

// Opt-in

#[test]
fn opted_in_package_is_silent() {
    let options = CheckOptions {
        experimental_packages: BTreeSet::from([PACKAGE.to_string()]),
    };
    let checked = check_packages_with(CROSS_PACKAGE, ParseMode::File, &options)
        .expect("cross-package fixture typechecks");
    let warnings = warning_messages(&checked);
    assert!(
        warnings.is_empty(),
        "an opted-in package must not warn: {warnings:?}"
    );
}

#[test]
fn opt_in_covers_only_the_named_package() {
    let files: Vec<(&str, &str, &str)> = CROSS_PACKAGE
        .iter()
        .copied()
        .chain([(
            "Other",
            "other.koja",
            "
            fn read(s: Lib.Span) -> Int
              s.id
            end
            ",
        )])
        .collect();
    let options = CheckOptions {
        experimental_packages: BTreeSet::from([PACKAGE.to_string()]),
    };
    let checked =
        check_packages_with(&files, ParseMode::File, &options).expect("fixture typechecks");
    let warnings: Vec<_> = checked
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .collect();
    assert_eq!(warnings.len(), 1, "only `Other` warns: {warnings:#?}");
    assert_eq!(
        checked.path_of(warnings[0].span.file),
        Some(Path::new("other.koja"))
    );
}

#[test]
fn opt_in_does_not_silence_deprecation() {
    let options = CheckOptions {
        experimental_packages: BTreeSet::from([PACKAGE.to_string()]),
    };
    let checked = check_packages_with(
        &[(
            PACKAGE,
            "app.koja",
            "
            @deprecated \"Use add instead.\"
            fn old_add(a: Int, b: Int) -> Int
              a + b
            end

            fn run() -> Int
              old_add(1, 2)
            end
            ",
        )],
        ParseMode::File,
        &options,
    )
    .expect("fixture typechecks");
    assert_warns(
        &warning_messages(&checked),
        &["`old_add` is deprecated. Use add instead."],
    );
}

// Suppression

#[test]
fn experimental_function_body_does_not_warn() {
    let source = "
        @experimental
        fn outer() -> Int
          inner()
        end

        @experimental
        fn inner() -> Int
          1
        end
        ";
    let warnings = warning_messages(&typecheck_file(&dedent(source)));
    assert!(
        warnings.is_empty(),
        "experimental bodies must not warn about experimental uses: {warnings:?}",
    );
}

#[test]
fn experimental_struct_members_do_not_warn() {
    let source = "
        @experimental
        struct Span
          id: Int

          fn root() -> Span
            Span{id: 0}
          end
        end
        ";
    let warnings = warning_messages(&typecheck_file(&dedent(source)));
    assert!(
        warnings.is_empty(),
        "an experimental type's own members must not warn: {warnings:?}",
    );
}

#[test]
fn extend_on_experimental_target_does_not_warn() {
    let source = "
        @experimental
        struct Span
          id: Int
        end

        extend Span
          fn double(self) -> Int
            Span{id: self.id * 2}.id
          end
        end
        ";
    let warnings = warning_messages(&typecheck_file(&dedent(source)));
    assert!(
        warnings.is_empty(),
        "extend blocks on an experimental target must not warn: {warnings:?}",
    );
}

#[test]
fn impl_on_experimental_target_does_not_warn() {
    let source = "
        protocol Show
          fn show(self) -> String
        end

        @experimental
        struct Span
          id: Int
        end

        impl Show for Span
          fn show(self) -> String
            \"span\"
          end
        end
        ";
    let warnings = warning_messages(&typecheck_file(&dedent(source)));
    assert!(
        warnings.is_empty(),
        "impl blocks on an experimental target must not warn: {warnings:?}",
    );
}

// Diagnostic file attribution

#[test]
fn error_diagnostics_carry_the_owning_file_path() {
    let source = "
        @experimental false
        fn trace() -> Int
          1
        end
        ";
    let failure = typecheck_script_fail(&dedent(source));
    let error = failure
        .diagnostics
        .first()
        .expect("expected a placement error");
    assert_eq!(
        failure.path_of(error.span.file),
        Some(Path::new("test.koja"))
    );
}
