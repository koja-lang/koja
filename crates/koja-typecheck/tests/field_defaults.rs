//! Default field values: declaration-time validation (shape, names,
//! types) and construction-time fill for omitted fields, on structs
//! and enum struct variants, within and across packages.

use koja_ast::ast::{EnumConstructionData, Expr, ExprKind, FieldInit, Name};
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

/// Every span inside a synthesized fill is synthetic: the nodes, the
/// field init names, and the type path and variant names. Position
/// lookups and the reference index would otherwise attribute the
/// declaration's default to each site that omits the field.
#[test]
fn synthesized_fill_is_synthetic_all_the_way_down() {
    let source = "
        enum Shape
          Circle
          Rect{width: Int, height: Int}
        end

        struct Frame
          x: Int
        end

        struct Canvas
          frame: Frame = Frame{x: -1}
          shapes: List<Shape> = [Shape.Rect{width: 1, height: 2}, Shape.Circle]
        end

        Canvas{}
        ";
    let checked = typecheck(&dedent(source));
    let ExprKind::StructConstruction { fields, .. } = &trailing_expr(&checked).kind else {
        panic!("expected trailing struct construction");
    };
    assert_eq!(fields.len(), 2, "both fields should be synthesized");
    for field in fields {
        assert_field_init_synthetic(field);
    }
}

fn assert_field_init_synthetic(field: &FieldInit) {
    assert!(field.span.synthetic, "init `{}` span", field.name);
    assert!(field.name.span.synthetic, "init `{}` name span", field.name);
    assert_expr_synthetic(&field.value);
}

fn assert_expr_synthetic(expr: &Expr) {
    assert!(expr.span.synthetic, "expression span: {:?}", expr.kind);
    match &expr.kind {
        ExprKind::EnumConstruction {
            type_path,
            variant,
            data,
        } => {
            assert_names_synthetic(type_path);
            assert!(variant.span.synthetic, "variant `{variant}` span");
            match data {
                EnumConstructionData::Struct(fields) => {
                    fields.iter().for_each(assert_field_init_synthetic);
                }
                EnumConstructionData::Tuple(elements) => {
                    elements.iter().for_each(assert_expr_synthetic);
                }
                EnumConstructionData::Unit => {}
            }
        }
        ExprKind::List { elements } => elements.iter().for_each(assert_expr_synthetic),
        ExprKind::Literal { .. } => {}
        ExprKind::StructConstruction { type_path, fields } => {
            assert_names_synthetic(type_path);
            fields.iter().for_each(assert_field_init_synthetic);
        }
        ExprKind::Unary { operand, .. } => assert_expr_synthetic(operand),
        other => panic!("unexpected node in a synthesized fill: {other:?}"),
    }
}

fn assert_names_synthetic(names: &[Name]) {
    for name in names {
        assert!(name.span.synthetic, "name `{name}` span");
    }
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
fn aliased_name_in_default_resolves_at_every_site() {
    // The default is spelled through the declaring file's alias. A
    // construction in that file, and one in a third package with no
    // such alias, both fill it.
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

                fn local() -> Theme
                  Theme{}
                end
                ",
            ),
            (
                "Consumer",
                "consumer.koja",
                "
                fn remote() -> TestApp.Theme
                  TestApp.Theme{}
                end
                ",
            ),
        ],
        ParseMode::File,
    );
    result.expect("aliased default should resolve at the declaration and at each site");
}

#[test]
fn dotted_same_package_struct_default_fills() {
    let source = "
        struct Outer.Opts
          retries: Int = 3
        end

        struct Outer
          opts: Outer.Opts = Outer.Opts{}
        end

        Outer{}
        ";
    let checked = typecheck(&dedent(source));
    let ExprKind::StructConstruction { fields, .. } = &trailing_expr(&checked).kind else {
        panic!("expected trailing struct construction");
    };
    assert_eq!(fields.len(), 1, "the dotted struct literal should fill");
    assert!(
        matches!(fields[0].value.kind, ExprKind::StructConstruction { .. }),
        "the synthesized default should resolve as a struct construction",
    );
}

#[test]
fn cross_package_struct_literal_default_fills() {
    let result = check_packages(
        &[
            (
                "Lib",
                "lib.koja",
                "
                struct Tracer
                  ref: Option<Int>
                end
                ",
            ),
            (
                PACKAGE,
                "main.koja",
                "
                struct Trace
                  tracer: Lib.Tracer = Lib.Tracer{ref: Option.None}
                end
                ",
            ),
            (
                "Consumer",
                "consumer.koja",
                "
                fn build() -> TestApp.Trace
                  TestApp.Trace{}
                end
                ",
            ),
        ],
        ParseMode::File,
    );
    result.expect("a struct literal of another package's type should be a default");
}

#[test]
fn enum_payload_variant_defaults_fill() {
    let source = "
        enum Shape
          Rect{width: Int, height: Int}
          Dot
        end

        struct Canvas
          bounds: Shape = Shape.Rect{width: 1, height: 2}
          limit: Option<Int> = Option.Some(3)
        end

        Canvas{}
        ";
    let checked = typecheck(&dedent(source));
    let ExprKind::StructConstruction { fields, .. } = &trailing_expr(&checked).kind else {
        panic!("expected trailing struct construction");
    };
    assert_eq!(fields.len(), 2, "both payload-variant defaults should fill");
}

#[test]
fn failed_default_with_omitting_site_diagnoses_once_at_the_declaration() {
    let result = check_packages(
        &[(
            PACKAGE,
            "main.koja",
            "
            struct Config
              mode: Missing = Missing.Fast
            end

            fn build() -> Config
              Config{}
            end
            ",
        )],
        ParseMode::File,
    );
    let failure = result.expect_err("an unknown name in a default should diagnose");
    let messages = diagnostic_messages(&failure);
    assert!(
        !messages.iter().any(|m| m.contains("missing field `mode`")),
        "the site should stay quiet when the declaration already failed, got: {messages:#?}",
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
fn cross_package_constant_default_holds_generic_unit_variant() {
    let result = check_packages(
        &[
            (
                "Lib",
                "lib.koja",
                "
                struct Tracer
                  ref: Option<Int>

                  const NOOP: Tracer = Tracer{ref: Option.None}
                end
                ",
            ),
            (
                PACKAGE,
                "main.koja",
                "
                struct Trace
                  tracer: Lib.Tracer = Lib.Tracer.NOOP
                end

                fn build() -> Trace
                  Trace{}
                end
                ",
            ),
        ],
        ParseMode::File,
    );
    result.expect("a package constant holding `Option.None` should be a consumer's default");
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
