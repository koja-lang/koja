//! Default field values: declaration-time validation (shape, names,
//! types) and construction-time fill for omitted fields, on structs
//! and enum struct variants, within and across packages.

use koja_ast::ast::ExprKind;
use koja_ast::util::dedent;
use koja_parser::ParseMode;

mod common;

use common::{
    PACKAGE, assert_script_fails_with, check_packages, diagnostic_messages, trailing_expr,
    typecheck_script as typecheck, warning_messages,
};

#[test]
fn omitted_fields_fill_from_defaults() {
    let source = "
        struct Config
          host: String = \"localhost\"
          port: Int = 5432
          name: String
        end

        Config{name: \"app\"}
        ";
    let checked = typecheck(&dedent(source));
    let ExprKind::StructConstruction { fields, .. } = &trailing_expr(&checked).kind else {
        panic!("expected trailing struct construction");
    };
    assert_eq!(fields.len(), 3, "omitted fields should be synthesized");
    let host = fields.iter().find(|f| f.name == "host").unwrap();
    assert!(host.span.synthetic, "synthesized init spans are synthetic");
    assert!(host.value.resolution.is_resolved());
}

#[test]
fn explicit_init_overrides_default() {
    let source = "
        struct Config
          port: Int = 5432
        end

        Config{port: 9000}
        ";
    let checked = typecheck(&dedent(source));
    let ExprKind::StructConstruction { fields, .. } = &trailing_expr(&checked).kind else {
        panic!("expected trailing struct construction");
    };
    assert_eq!(fields.len(), 1);
    assert!(!fields[0].span.synthetic);
}

#[test]
fn all_default_construction_typechecks() {
    let source = "
        struct Point
          x: Int = 0
          y: Int = 0
        end

        Point{}
        ";
    typecheck(&dedent(source));
}

#[test]
fn generic_fields_take_none_and_empty_list_defaults() {
    let source = "
        struct Stack<T>
          items: List<T> = []
          top: Option<T> = Option.None
        end

        s: Stack<Int> = Stack{}
        s.items.length()
        ";
    typecheck(&dedent(source));
}

#[test]
fn missing_field_without_default_still_diagnoses() {
    assert_script_fails_with(
        "
        struct Config
          host: String = \"localhost\"
          name: String
        end

        Config{}
        ",
        &["missing field `name` in literal for `TestApp.Config`"],
    );
}

#[test]
fn default_type_mismatch_diagnoses() {
    assert_script_fails_with(
        "
        struct Config
          port: Int = \"hi\"
        end

        0
        ",
        &["default for field `port` of `TestApp.Config` expects `Int`, got `String`"],
    );
}

#[test]
fn default_out_of_range_literal_diagnoses() {
    assert_script_fails_with(
        "
        struct Flags
          mask: UInt8 = 300
        end

        0
        ",
        &["default for field `mask` of `TestApp.Flags` expects `UInt8`"],
    );
}

#[test]
fn function_call_default_diagnoses() {
    assert_script_fails_with(
        "
        fn compute() -> Int
          1
        end

        struct Config
          port: Int = compute()
        end

        0
        ",
        &["default field values are limited to literals"],
    );
}

#[test]
fn interpolated_string_default_diagnoses() {
    assert_script_fails_with(
        "
        struct Config
          host: String = \"a #{1} b\"
        end

        0
        ",
        &["interpolated strings are not allowed in default field values"],
    );
}

#[test]
fn unknown_name_in_default_diagnoses() {
    assert_script_fails_with(
        "
        struct Config
          mode: Missing = Missing.Fast
        end

        0
        ",
        &["typecheck does not recognize the enum type `Missing`"],
    );
}

#[test]
fn aliased_name_in_default_diagnoses_with_hint() {
    let result = check_packages(
        &[
            (
                "Lib",
                "lib.koja",
                "
                enum Color
                  Red
                  Blue
                end
                ",
            ),
            (
                PACKAGE,
                "main.koja",
                "
                alias Lib.Color

                struct Theme
                  accent: Color = Color.Red
                end
                ",
            ),
        ],
        ParseMode::File,
    );
    let failure = result.expect_err("aliased default should diagnose");
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains(
            "default for field `accent` of `TestApp.Theme` cannot use an `alias` shorthand"
        )),
        "expected the qualified-name hint, got: {messages:#?}",
    );
}

#[test]
fn cross_package_default_resolves_in_declaring_package() {
    let result = check_packages(
        &[
            (
                "Lib",
                "lib.koja",
                "
                enum Mode
                  Fast
                  Safe
                end

                struct Job
                  mode: Mode = Mode.Fast
                  retries: Int = 3
                end
                ",
            ),
            (
                PACKAGE,
                "main.koja",
                "
                fn build() -> Lib.Job
                  Lib.Job{}
                end
                ",
            ),
        ],
        ParseMode::File,
    );
    result.expect("cross-package defaulted construction should typecheck");
}

#[test]
fn enum_struct_variant_defaults_fill() {
    let source = "
        enum Shape
          Rect{width: Int, height: Int = 2}
          Dot
        end

        Shape.Rect{width: 4}
        ";
    let checked = typecheck(&dedent(source));
    let ExprKind::EnumConstruction { data, .. } = &trailing_expr(&checked).kind else {
        panic!("expected trailing enum construction");
    };
    let koja_ast::ast::EnumConstructionData::Struct(fields) = data else {
        panic!("expected struct-variant construction data");
    };
    assert_eq!(fields.len(), 2, "omitted variant field should fill");
}

#[test]
fn enum_struct_variant_default_mismatch_diagnoses() {
    assert_script_fails_with(
        "
        enum Shape
          Rect{width: Int = true}
        end

        0
        ",
        &["default for field `width` of `TestApp.Shape.Rect` expects `Int`, got `Bool`"],
    );
}

#[test]
fn deprecated_enum_in_default_warns_at_declaration_not_per_site() {
    // The declaration references `OldMode` twice (field type and
    // default value), so two warnings. The two constructions that
    // omit the field must not add any: their synthesized fills are
    // synthetic and the deprecation walker skips them.
    let source = "
        @deprecated \"Use Mode instead.\"
        enum OldMode
          Fast
        end

        struct Job
          mode: OldMode = OldMode.Fast
        end

        a = Job{}
        b = Job{}
        0
        ";
    let checked = typecheck(&dedent(source));
    let warnings = warning_messages(&checked);
    let hits = warnings
        .iter()
        .filter(|m| m.contains("OldMode") && m.contains("deprecated"))
        .count();
    assert_eq!(
        hits, 2,
        "expected declaration-only deprecation warnings, got: {warnings:#?}",
    );
}
