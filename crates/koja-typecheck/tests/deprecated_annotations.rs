//! `@deprecated "message"` marks a declaration as deprecated. The
//! message is required, and every use that resolves to the marked
//! entry warns at the use site. Uses inside the deprecated decl
//! itself and inside `impl` / `extend` blocks on a deprecated target
//! are suppressed.

use std::path::Path;

use koja_ast::ast::Severity;
use koja_ast::util::dedent;
use koja_parser::ParseMode;

mod common;

use common::{
    PACKAGE, assert_script_fails_with, check_packages, registry_id, typecheck_file,
    typecheck_script, typecheck_script_fail, warning_messages,
};

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

// Placement and payload validation

#[test]
fn bare_deprecated_is_rejected() {
    let source = "
        @deprecated
        fn old_add(a: Int, b: Int) -> Int
          a + b
        end

        old_add(1, 2)
        ";
    assert_script_fails_with(source, &["`@deprecated` requires a message"]);
}

#[test]
fn deprecated_false_is_rejected() {
    let source = "
        @deprecated false
        struct Old
          x: Int
        end
        ";
    assert_script_fails_with(source, &["`@deprecated` requires a message"]);
}

#[test]
fn deprecated_empty_message_is_rejected() {
    let source = "
        @deprecated \"  \"
        fn old_add(a: Int, b: Int) -> Int
          a + b
        end

        old_add(1, 2)
        ";
    assert_script_fails_with(source, &["`@deprecated` requires a message"]);
}

#[test]
fn deprecated_on_protocol_method_is_rejected() {
    let source = "
        protocol Show
          @deprecated \"Use render instead.\"
          fn show(self) -> String
        end
        ";
    assert_script_fails_with(source, &["annotations on protocol methods", "@deprecated"]);
}

#[test]
fn deprecated_on_priv_decl_is_accepted() {
    let source = "
        @deprecated \"Use the public API instead.\"
        priv struct Hidden
          slot: Int
        end
        ";
    let warnings = script_warnings(source);
    assert!(
        warnings.is_empty(),
        "unused decl must not warn: {warnings:?}"
    );
}

#[test]
fn deprecated_is_accepted_on_every_decl_kind() {
    let source = "
        @deprecated \"Use New instead.\"
        struct Old
          x: Int
        end

        @deprecated \"Use Mode instead.\"
        enum Toggle
          Off
          On
        end

        @deprecated \"Use Render instead.\"
        protocol Show
          fn show(self) -> String
        end

        @deprecated \"Use MAX instead.\"
        const LIMIT: Int = 10

        @deprecated \"Use Cat directly.\"
        type Pet = Old

        @deprecated \"Use add instead.\"
        fn old_add(a: Int, b: Int) -> Int
          a + b
        end
        ";
    typecheck_file(&dedent(source));
}

#[test]
fn message_is_stamped_on_the_registry_entry() {
    let source = "
        @deprecated \"Use add instead.\"
        fn old_add(a: Int, b: Int) -> Int
          a + b
        end
        ";
    let checked = typecheck_file(&dedent(source));
    let id = registry_id(&checked, PACKAGE, &["old_add"]);
    let entry = checked.registry.get(id).expect("entry is registered");
    assert_eq!(entry.deprecation.as_deref(), Some("Use add instead."));
}

#[test]
fn multiline_message_is_trimmed() {
    let source = "
        @deprecated \"\"\"
        Use `add` instead.
        \"\"\"
        fn old_add(a: Int, b: Int) -> Int
          a + b
        end

        old_add(1, 2)
        ";
    let warnings = script_warnings(source);
    assert_warns(&warnings, &["`old_add` is deprecated. Use `add` instead."]);
}

// Use-site warnings

#[test]
fn call_to_deprecated_function_warns() {
    let warnings = script_warnings(
        "
        @deprecated \"Use add instead.\"
        fn old_add(a: Int, b: Int) -> Int
          a + b
        end

        old_add(1, 2)
        ",
    );
    assert_warns(&warnings, &["`old_add` is deprecated. Use add instead."]);
}

#[test]
fn deprecated_type_in_signature_position_warns() {
    let warnings = script_warnings(
        "
        @deprecated \"Use New instead.\"
        struct Old
          x: Int
        end

        fn read(o: Old) -> Int
          o.x
        end
        ",
    );
    assert_warns(&warnings, &["`Old` is deprecated. Use New instead."]);
}

#[test]
fn construction_of_deprecated_struct_warns() {
    let warnings = script_warnings(
        "
        @deprecated \"Use New instead.\"
        struct Old
          x: Int
        end

        o = Old{x: 1}
        o.x
        ",
    );
    assert_warns(&warnings, &["`Old` is deprecated. Use New instead."]);
}

#[test]
fn deprecated_enum_construction_and_pattern_warn() {
    let warnings = script_warnings(
        "
        @deprecated \"Use Mode instead.\"
        enum Toggle
          Off
          On
        end

        t = Toggle.Off
        match t
          Toggle.Off -> 0
          Toggle.On -> 1
        end
        ",
    );
    let hits = warnings
        .iter()
        .filter(|w| w.contains("`Toggle` is deprecated. Use Mode instead."))
        .count();
    assert!(
        hits >= 3,
        "expected construction + two pattern warnings, got: {warnings:#?}",
    );
}

#[test]
fn deprecated_constant_read_warns() {
    let warnings = script_warnings(
        "
        @deprecated \"Use MAX instead.\"
        const LIMIT: Int = 10

        LIMIT + 1
        ",
    );
    assert_warns(&warnings, &["`LIMIT` is deprecated. Use MAX instead."]);
}

#[test]
fn static_call_on_deprecated_type_warns() {
    let warnings = script_warnings(
        "
        @deprecated \"Use New instead.\"
        struct Old
          x: Int

          fn origin() -> Old
            Old{x: 0}
          end
        end

        Old.origin()
        ",
    );
    assert_warns(&warnings, &["`Old` is deprecated. Use New instead."]);
}

#[test]
fn call_to_deprecated_method_warns() {
    let warnings = script_warnings(
        "
        struct Point
          x: Int

          @deprecated \"Use shift instead.\"
          fn legacy(self) -> Int
            self.x
          end
        end

        p = Point{x: 1}
        p.legacy()
        ",
    );
    assert_warns(
        &warnings,
        &["`Point.legacy` is deprecated. Use shift instead."],
    );
}

#[test]
fn deprecated_type_alias_use_warns() {
    let warnings = script_warnings(
        "
        struct Cat
          name: String
        end

        @deprecated \"Use Cat directly.\"
        type Pet = Cat

        fn feed(pet: Pet) -> Pet
          pet
        end
        ",
    );
    assert_warns(&warnings, &["`Pet` is deprecated. Use Cat directly."]);
}

#[test]
fn deprecated_protocol_bound_warns() {
    let warnings = script_warnings(
        "
        @deprecated \"Use Render instead.\"
        protocol Show
          fn show(self) -> String
        end

        fn describe<T: Show>(value: T) -> String
          value.show()
        end
        ",
    );
    assert_warns(&warnings, &["`Show` is deprecated. Use Render instead."]);
}

#[test]
fn deprecated_parameterized_bound_argument_warns() {
    let warnings = script_warnings(
        "
        @deprecated \"Use NewItem instead.\"
        struct OldItem
          value: Int
        end

        protocol Source<T>
          fn first(self) -> T
        end

        fn describe<E: Source<OldItem>>(value: E) -> Int
          0
        end
        ",
    );
    assert_warns(
        &warnings,
        &["`OldItem` is deprecated. Use NewItem instead."],
    );
}

#[test]
fn cross_package_use_warns() {
    let checked = check_packages(
        &[
            (
                "Lib",
                "lib.koja",
                "
                @deprecated \"Use New instead.\"
                struct Old
                  x: Int
                end
                ",
            ),
            (
                PACKAGE,
                "app.koja",
                "
                fn read(o: Lib.Old) -> Int
                  o.x
                end
                ",
            ),
        ],
        ParseMode::File,
    )
    .expect("cross-package fixture typechecks");
    assert_warns(
        &warning_messages(&checked),
        &["`Old` is deprecated. Use New instead."],
    );
}

// Suppression

#[test]
fn deprecated_function_body_does_not_warn() {
    let source = "
        @deprecated \"Use modern instead.\"
        fn ancient() -> Int
          legacy()
        end

        @deprecated \"Use modern instead.\"
        fn legacy() -> Int
          1
        end

        fn modern() -> Int
          2
        end
        ";
    let warnings = warning_messages(&typecheck_file(&dedent(source)));
    assert!(
        warnings.is_empty(),
        "deprecated bodies must not warn about deprecated uses: {warnings:?}",
    );
}

#[test]
fn deprecated_struct_members_do_not_warn() {
    let source = "
        @deprecated \"Use New instead.\"
        struct Old
          x: Int

          fn origin() -> Old
            Old{x: 0}
          end
        end
        ";
    let warnings = warning_messages(&typecheck_file(&dedent(source)));
    assert!(
        warnings.is_empty(),
        "a deprecated type's own members must not warn: {warnings:?}",
    );
}

#[test]
fn extend_on_deprecated_target_does_not_warn() {
    let source = "
        @deprecated \"Use New instead.\"
        struct Old
          x: Int
        end

        extend Old
          fn double(self) -> Int
            Old{x: self.x * 2}.x
          end
        end
        ";
    let warnings = warning_messages(&typecheck_file(&dedent(source)));
    assert!(
        warnings.is_empty(),
        "extend blocks on a deprecated target must not warn: {warnings:?}",
    );
}

#[test]
fn impl_on_deprecated_target_does_not_warn() {
    let source = "
        protocol Show
          fn show(self) -> String
        end

        @deprecated \"Use New instead.\"
        struct Old
          x: Int
        end

        impl Show for Old
          fn show(self) -> String
            \"old\"
          end
        end
        ";
    let warnings = warning_messages(&typecheck_file(&dedent(source)));
    assert!(
        warnings.is_empty(),
        "impl blocks on a deprecated target must not warn: {warnings:?}",
    );
}

// Diagnostic file attribution

#[test]
fn warnings_carry_the_owning_file_path() {
    let source = "
        @deprecated \"Use new_add instead.\"
        fn old_add(a: Int, b: Int) -> Int
          a + b
        end

        old_add(1, 2)
        ";
    let checked = typecheck_script(&dedent(source));
    let warning = checked
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Warning)
        .expect("expected a deprecation warning");
    assert_eq!(
        checked.path_of(warning.span.file),
        Some(Path::new("test.koja"))
    );
}

#[test]
fn cross_package_warnings_carry_the_using_file_path() {
    let checked = check_packages(
        &[
            (
                "Lib",
                "lib.koja",
                "
                @deprecated \"Use New instead.\"
                struct Old
                  x: Int
                end
                ",
            ),
            (
                PACKAGE,
                "app.koja",
                "
                fn read(o: Lib.Old) -> Int
                  o.x
                end
                ",
            ),
        ],
        ParseMode::File,
    )
    .expect("cross-package fixture typechecks");
    let warning = checked
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Warning)
        .expect("expected a deprecation warning");
    assert_eq!(
        checked.path_of(warning.span.file),
        Some(Path::new("app.koja"))
    );
}

#[test]
fn error_diagnostics_carry_the_owning_file_path() {
    let source = "
        @deprecated
        fn old_add(a: Int, b: Int) -> Int
          a + b
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
