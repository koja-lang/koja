//! Package-level `const` lifting: literals, enums, structs, list, map,
//! and set literals, annotation matching, interpolation / non-literal
//! RHS rejection, constants that read other constants in any source
//! order, dependency cycles, duplicate names, immutability (no
//! assigning to constants from function bodies), and package-qualified
//! reads across package boundaries.

use koja_ast::ast::ExprKind;
use koja_ast::identifier::{Resolution, ResolvedType};
use koja_ast::util::dedent;
use koja_parser::ParseMode;
use koja_typecheck::{CheckFailure, CheckedProgram, ConstantDefinition, GlobalKind};

mod common;

use common::{
    PACKAGE, assert_script_fails_with, check_packages, diagnostic_messages, global_named, int_type,
    registry_id, string_type, typecheck_script as typecheck, typecheck_script_fail,
    warning_messages,
};

/// The stamped definition of the `TestApp` constant at `path`.
fn constant_definition<'a>(checked: &'a CheckedProgram, path: &[&str]) -> &'a ConstantDefinition {
    let id = registry_id(checked, PACKAGE, path);
    let entry = checked.registry.get(id).expect("constant id is live");
    match &entry.kind {
        GlobalKind::Constant(Some(definition)) => definition,
        other => panic!("expected a lifted constant at `{path:?}`, got {other:?}"),
    }
}

fn list_of(checked: &CheckedProgram, element: ResolvedType) -> ResolvedType {
    global_named(checked, "List", vec![element])
}

#[test]
fn primitive_string_and_struct_literal_constants_typecheck() {
    let source = "
        enum Direction
          North
        end

        struct Point
          x: Int
          y: Int
        end

        const N = 7
        const GREETING = \"hi\"
        const HEADING = Direction.North
        const ORIGIN = Point{x: 1, y: 2}

        N
        ";
    typecheck(&dedent(source));
}

#[test]
fn generic_unit_variant_constants_take_annotation_type_args() {
    let source = "
        struct Simple
          ref: Option<Int>

          const EMPTY: Simple = Simple{ref: Option.None}
        end

        const NOTHING: Option<Int> = Option.None

        Simple.EMPTY
        ";
    typecheck(&dedent(source));
}

#[test]
fn generic_unit_variant_constant_peels_alias_annotation() {
    let source = "
        type Maybe = Option<Int>

        const NOTHING: Maybe = Option.None

        0
        ";
    let checked = typecheck(&dedent(source));

    let definition = constant_definition(&checked, &["NOTHING"]);
    assert_eq!(
        definition.value.resolution,
        global_named(&checked, "Option", vec![int_type(&checked)])
    );
}

#[test]
fn unannotated_generic_unit_variant_constant_diagnoses() {
    let source = "
        const NOTHING = Option.None

        0
        ";

    assert_script_fails_with(
        source,
        &["cannot infer type parameter `T` of `Global.Option` from unit variant `None`"],
    );
}

#[test]
fn generic_unit_variant_against_other_type_diagnoses() {
    // The annotation can never hold an `Option`, so the resolver
    // names the mismatch instead of the inference gap it causes.
    let source = "
        const NOTHING: String = Option.None

        0
        ";

    assert_script_fails_with(
        source,
        &["`Option.None` is a `Global.Option` value, but `String` is expected"],
    );
}

#[test]
fn constant_mismatch_renders_type_arguments() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        const P: Option<Int> = Point{x: 0, y: 0}

        0
        ";

    assert_script_fails_with(
        source,
        &[
            "constant value type `TestApp.Point` does not match annotation `Global.Option<Global.Int>`",
        ],
    );
}

#[test]
fn constant_annotation_mismatch_diagnoses() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        const P: String = Point{x: 0, y: 0}

        0
        ";

    assert_script_fails_with(
        source,
        &["constant value type", "does not match annotation"],
    );
}

// Collection constants. The value resolves through the body
// resolver, so list and map literals infer their element types, a
// `Set` annotation dispatches through the `Set.from_list` carrier
// rewrite, and the diagnostics match what a body binding reports.

#[test]
fn list_and_map_constants_infer_their_type() {
    let checked = typecheck(&dedent(
        "
        const PRIMES = [2, 3, 5]
        const PORTS = [\"http\": 80, \"https\": 443]

        PRIMES
        ",
    ));

    let int = int_type(&checked);
    assert_eq!(
        constant_definition(&checked, &["PRIMES"]).ty,
        list_of(&checked, int.clone())
    );
    assert_eq!(
        constant_definition(&checked, &["PORTS"]).ty,
        global_named(&checked, "Map", vec![string_type(&checked), int])
    );
}

#[test]
fn set_annotation_stores_a_from_list_call() {
    let checked = typecheck(&dedent(
        "
        const TAGS: Set<String> = [\"a\", \"b\"]

        TAGS
        ",
    ));

    let set_of_string = global_named(&checked, "Set", vec![string_type(&checked)]);
    let definition = constant_definition(&checked, &["TAGS"]);
    assert_eq!(definition.ty, set_of_string);
    assert_eq!(definition.value.resolution, set_of_string);
    match &definition.value.kind {
        ExprKind::MethodCall { method, .. } => assert_eq!(method.text, "from_list"),
        other => panic!("expected the carrier rewrite to a `from_list` call, got {other:?}"),
    }
}

#[test]
fn nested_list_in_struct_constant_typechecks() {
    let checked = typecheck(&dedent(
        "
        struct Config
          name: String
          ports: List<Int>
        end

        const DEFAULT = Config{name: \"web\", ports: [80, 443]}

        DEFAULT
        ",
    ));

    let ExprKind::StructConstruction { fields, .. } =
        &constant_definition(&checked, &["DEFAULT"]).value.kind
    else {
        panic!("expected a struct construction value");
    };
    let ports = fields
        .iter()
        .find(|field| field.name.text == "ports")
        .expect("`ports` field is present");
    assert_eq!(
        ports.value.resolution,
        list_of(&checked, int_type(&checked))
    );
}

#[test]
fn empty_collection_constants_without_annotation_diagnose() {
    assert_script_fails_with(
        "
        const NOTHING = []

        0
        ",
        &["`[]` has no element type"],
    );
    assert_script_fails_with(
        "
        const NOTHING = [:]

        0
        ",
        &["`[]` has no key type"],
    );
}

#[test]
fn list_element_mismatch_diagnoses() {
    assert_script_fails_with(
        "
        const MIXED: List<Int> = [1, \"two\"]

        0
        ",
        &["list literal element type mismatch. Expected `Int`, found `String`"],
    );
}

#[test]
fn list_against_scalar_annotation_reports_once() {
    let failure = typecheck_script_fail(&dedent(
        "
        const L: Int = [1]

        0
        ",
    ));

    let messages = diagnostic_messages(&failure);
    assert_eq!(
        messages.len(),
        1,
        "expected one diagnostic, got {messages:#?}"
    );
    assert!(
        messages[0].contains("does not match annotation `Global.Int`"),
        "unexpected diagnostic: {}",
        messages[0]
    );
}

#[test]
fn payload_variant_constant_typechecks() {
    let checked = typecheck(&dedent(
        "
        enum Shape
          Dot
          Circle(Float)
        end

        const UNIT = Shape.Circle(1.0)

        UNIT
        ",
    ));

    let shape = registry_id(&checked, PACKAGE, &["Shape"]);
    assert_eq!(
        constant_definition(&checked, &["UNIT"]).ty,
        ResolvedType::leaf(Resolution::Global(shape))
    );
}

#[test]
fn struct_constant_fills_omitted_field_defaults() {
    typecheck(&dedent(
        "
        struct Point
          x: Int = 0
          y: Int = 0
        end

        const ORIGIN = Point{}

        ORIGIN
        ",
    ));
}

// Constants that read other constants. The lift runs in dependency
// order, so a constant can read one declared below it whether the
// read is bare, `Owner.NAME`, or `Package.NAME`.

#[test]
fn constant_reads_constant_declared_below_it() {
    let checked = typecheck(&dedent(
        "
        const BARE = BASE
        const OWNED = Limits.MAX
        const QUALIFIED = TestApp.BASE
        const BASE = 7

        struct Limits
          value: Int

          const MAX = BASE
        end

        BARE
        ",
    ));

    let int = int_type(&checked);
    for path in [
        &["BARE"][..],
        &["OWNED"],
        &["QUALIFIED"],
        &["Limits", "MAX"],
    ] {
        assert_eq!(constant_definition(&checked, path).ty, int, "{path:?}");
    }
}

#[test]
fn constant_reads_default_of_omitted_struct_field() {
    // `Point{}` reads `ORIGIN_X` through the omitted field's default,
    // so the lift has to resolve `ORIGIN_X` first even though it is
    // declared last.
    typecheck(&dedent(
        "
        struct Point
          x: Int = ORIGIN_X
          y: Int = 0
        end

        const ORIGIN = Point{}
        const ORIGIN_X = 0

        ORIGIN
        ",
    ));
}

#[test]
fn constant_reads_cross_package_constant() {
    check_lib_and_app(
        "
        const LIMIT = Lib.MAX

        LIMIT.print()
        ",
    )
    .expect("a constant can read a public constant from another package");
}

#[test]
fn two_constant_cycle_diagnoses_both() {
    let failure = typecheck_script_fail(&dedent(
        "
        const A = B
        const B = A

        0
        ",
    ));

    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m == "constant `A` depends on itself through `B`"),
        "got {messages:#?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m == "constant `B` depends on itself through `A`"),
        "got {messages:#?}"
    );
}

#[test]
fn self_referential_constant_diagnoses() {
    assert_script_fails_with(
        "
        const LOOP = LOOP

        0
        ",
        &["constant `LOOP` depends on itself"],
    );
}

#[test]
fn constant_downstream_of_a_cycle_diagnoses() {
    assert_script_fails_with(
        "
        const A = B
        const B = A
        const C = A

        0
        ",
        &["constant `C` depends on `A`, which is in a dependency cycle"],
    );
}

#[test]
fn non_literal_rhs_diagnoses() {
    let source = "
        const X = 1 + 1

        X
        ";

    assert_script_fails_with(source, &["constant values are limited to literals"]);
}

#[test]
fn interpolated_string_constant_diagnoses() {
    let source = "
        const S = \"a #{7} b\"

        S
        ";

    assert_script_fails_with(
        source,
        &["interpolated strings are not allowed in constant values"],
    );
}

#[test]
fn binary_literal_constants_typecheck() {
    let source = "
        const SYNC: Binary = <<0x53::8, 4::32>>
        const GREETING = <<\"hi\", 0::8>>
        const FLAGS: Bits = <<5::3>>

        SYNC
        ";
    typecheck(&dedent(source));
}

#[test]
fn binary_constant_with_non_literal_segment_diagnoses() {
    let source = "
        const TAG = 5
        const FRAME: Binary = <<TAG::8>>

        FRAME
        ";

    assert_script_fails_with(
        source,
        &["binary segment values in constant values must be literals"],
    );
}

#[test]
fn binary_constant_segment_out_of_range_diagnoses() {
    let source = "
        const FRAME: Binary = <<300::8>>

        FRAME
        ";

    assert_script_fails_with(source, &["does not fit in 8 unsigned bits"]);
}

#[test]
fn binary_constant_segment_kind_mismatch_diagnoses() {
    // A float-annotated segment folds a float literal's bits and
    // nothing else. resolve_segment already rejects `1.5::8` on its
    // own, so the int-into-float direction is the interesting one.
    let source = "
        const FRAME: Binary = <<7: Float32>>

        FRAME
        ";

    assert_script_fails_with(source, &["does not match the segment's declared shape"]);
}

#[test]
fn bits_valued_binary_constant_with_binary_annotation_diagnoses() {
    let source = "
        const FLAGS: Binary = <<5::3>>

        FLAGS
        ";

    assert_script_fails_with(
        source,
        &["constant value type", "does not match annotation"],
    );
}

#[test]
fn duplicate_constant_collides_like_other_globals() {
    let source = "
        const SAME = 1
        const SAME = 2

        SAME
        ";

    assert_script_fails_with(source, &["already defined"]);
}

#[test]
fn assignment_cannot_use_package_constant_as_lhs() {
    let source = "
        const PI = 3.14

        PI = 5.0
        0
        ";

    assert_script_fails_with(source, &["package-level constants", "immutable"]);
}

#[test]
fn compound_assign_on_package_constant_diagnoses() {
    let source = "
        const STEP = 1

        STEP += 2
        0
        ";

    assert_script_fails_with(source, &["immutable", "STEP"]);
}

// Package-qualified reads. `Lib.MAX` parses as a unit enum
// construction and `Lib.default_size` as a field access, so both
// surface shapes are covered.

const LIB_CONSTANTS: &str = "
    const MAX = 100
    const default_size = 25
    priv const HIDDEN = 7

    @deprecated \"Use MAX instead.\"
    const OLD_MAX = 50

    fn helper() -> Int
      1
    end

    priv fn hidden_helper() -> Int
      2
    end

    fn identity<T>(x: T) -> T
      x
    end

    struct Widget
      size: Int
    end
    ";

fn check_lib_and_app(app: &str) -> Result<CheckedProgram, CheckFailure> {
    check_packages(
        &[
            ("Lib", "lib.koja", LIB_CONSTANTS),
            (PACKAGE, "main.kojs", app),
        ],
        ParseMode::Script,
    )
}

fn assert_app_fails_with(app: &str, needle: &str) {
    let failure = check_lib_and_app(app).expect_err("expected a diagnostic");
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains(needle)),
        "expected `{needle}`, got {messages:?}",
    );
}

#[test]
fn public_constants_readable_cross_package() {
    check_lib_and_app(
        "
        total: Int = Lib.MAX + Lib.default_size
        total.print()
        ",
    )
    .expect("public cross-package constant reads should succeed");
}

#[test]
fn qualified_read_within_own_package() {
    typecheck(&dedent(
        "
        const MAX = 100

        TestApp.MAX.print()
        ",
    ));
}

#[test]
fn priv_constant_rejected_cross_package() {
    assert_app_fails_with(
        "Lib.HIDDEN.print()",
        "private constant `Lib.HIDDEN` cannot be referenced from package `TestApp`",
    );
}

#[test]
fn deprecated_constant_warns_at_qualified_read() {
    let checked =
        check_lib_and_app("Lib.OLD_MAX.print()").expect("deprecated reads still typecheck");
    let warnings = warning_messages(&checked);
    assert!(
        warnings
            .iter()
            .any(|m| m.contains("`OLD_MAX` is deprecated")),
        "expected a deprecation warning, got {warnings:?}",
    );
}

#[test]
fn unknown_member_in_known_package_diagnoses() {
    assert_app_fails_with(
        "Lib.MAXX.print()",
        "package `Lib` has no constant or function `MAXX`",
    );
}

#[test]
fn function_value_readable_cross_package() {
    check_lib_and_app(
        "
        f = &Lib.helper/0
        result: Int = f()
        result.print()
        ",
    )
    .expect("cross-package function values should typecheck");
}

#[test]
fn priv_function_value_rejected_cross_package() {
    assert_app_fails_with(
        "f = &Lib.hidden_helper/0\nf().print()",
        "private function `Lib.hidden_helper` cannot be referenced from package `TestApp`",
    );
}

#[test]
fn generic_function_value_diagnoses() {
    assert_app_fails_with(
        "f = &Lib.identity/1\nf(1).print()",
        "cannot reference generic function `Lib.identity` directly",
    );
}

#[test]
fn type_member_read_diagnoses() {
    assert_app_fails_with("x = Lib.Widget\n0", "`Lib.Widget` is a struct, not a value");
}

#[test]
fn global_constants_resolve_bare() {
    typecheck(&dedent(
        "
        out: IO.Descriptor = STDOUT
        out.print()
        ",
    ));
}

#[test]
fn package_constant_shadows_global_constant() {
    typecheck(&dedent(
        "
        const STDOUT = 1

        shadowed: Int = STDOUT
        shadowed.print()
        ",
    ));
}

#[test]
fn enum_in_scope_wins_over_package_prefix() {
    check_lib_and_app(
        "
        enum Lib
          MAX
        end

        heading: Lib = Lib.MAX
        heading.print()
        ",
    )
    .expect("a type named like a package takes precedence over the package");
}
