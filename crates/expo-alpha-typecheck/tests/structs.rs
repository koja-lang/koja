//! Typecheck coverage for the alpha struct slice: declaration
//! registration, signature lifting, struct-literal construction,
//! field-access resolution, and static method dispatch. Includes
//! per-feature gap diagnostics (generics, instance methods,
//! annotations, default field values, trait impls, type aliases in
//! impl blocks, impl on unknown / non-struct types) plus the
//! per-construction-site validation diagnostics (unknown / extra /
//! missing / duplicate / wrong-typed field, non-struct receiver).
//!
//! Static methods are tested in *both* declaration forms — inline in
//! the struct body and in an `impl` block — to pin that the two
//! surface forms produce identical registry entries and resolution
//! shape.

use expo_alpha_typecheck::{
    CheckedProgram, Dispatch, FunctionSignature, GlobalKind, ResolvedStructField, StructDefinition,
};
use expo_ast::ast::{Expr, ExprKind, Item, Statement, StructDecl};
use expo_ast::identifier::GlobalRegistryId;
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::util::dedent;

mod common;

use common::{
    PACKAGE, diagnostic_messages, typecheck_file as typecheck,
    typecheck_file_fail as typecheck_fail,
};

fn struct_definition<'a>(checked: &'a CheckedProgram, name: &str) -> &'a StructDefinition {
    let ident = Identifier::new(PACKAGE, vec![name.to_string()]);
    let (_, entry) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("`{ident}` not found in registry"));
    match &entry.kind {
        GlobalKind::Struct(Some(definition)) => definition,
        other => panic!("expected lifted Struct(Some(_)) for `{ident}`, got {other:?}"),
    }
}

fn find_struct_decl<'a>(checked: &'a CheckedProgram, name: &str) -> &'a StructDecl {
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("checked program is missing the test package");
    for file in &pkg.files {
        for item in &file.items {
            if let Item::Struct(decl) = item
                && decl.name == name
            {
                return decl;
            }
        }
    }
    panic!("struct `{name}` not found in checked program");
}

fn body_trailing_expr<'a>(checked: &'a CheckedProgram, fn_name: &str) -> &'a Expr {
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("checked program is missing the test package");
    for file in &pkg.files {
        for item in &file.items {
            if let Item::Function(function) = item
                && function.name == fn_name
            {
                let body = function.body.as_deref().expect("function has no body");
                return match body.last().expect("function body is empty") {
                    Statement::Expr(expr) => expr,
                    other => panic!("expected trailing Statement::Expr, got {other:?}"),
                };
            }
        }
    }
    panic!("fn `{fn_name}` not found in checked program");
}

fn global_leaf(checked: &CheckedProgram, name: &str) -> ResolvedType {
    let ident = Identifier::new("Global", vec![name.to_string()]);
    let (id, _) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("stdlib stub `Global.{name}` missing from registry"));
    ResolvedType::leaf(Resolution::Global(id))
}

fn package_leaf(checked: &CheckedProgram, name: &str) -> ResolvedType {
    let ident = Identifier::new(PACKAGE, vec![name.to_string()]);
    let (id, _) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("`{ident}` missing from registry"));
    ResolvedType::leaf(Resolution::Global(id))
}

fn lookup_function_signature<'a>(
    checked: &'a CheckedProgram,
    package: &str,
    path: &[&str],
) -> &'a FunctionSignature {
    let ident = Identifier::new(package, path.iter().map(|s| (*s).to_string()).collect());
    let (_, entry) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("`{ident}` not found in registry"));
    match &entry.kind {
        GlobalKind::Function(Some(sig)) => sig,
        other => panic!("expected lifted Function(Some(_)) for `{ident}`, got {other:?}"),
    }
}

fn lookup_struct_id(checked: &CheckedProgram, package: &str, name: &str) -> GlobalRegistryId {
    let ident = Identifier::new(package, vec![name.to_string()]);
    let (id, _) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("`{ident}` not found in registry"));
    id
}

// ---------------------------------------------------------------------------
// Decl registration / lift
// ---------------------------------------------------------------------------

#[test]
fn struct_decl_registers_with_lifted_definition() {
    let source = "
        struct Point
          x: Int
          y: Int
        end
        ";

    let checked = typecheck(&dedent(source));
    let definition = struct_definition(&checked, "Point");
    let int = global_leaf(&checked, "Int");

    assert_eq!(definition.fields.len(), 2);
    assert_eq!(definition.fields[0].name, "x");
    assert_eq!(definition.fields[0].ty, int);
    assert_eq!(definition.fields[1].name, "y");
    assert_eq!(definition.fields[1].ty, int);

    let decl = find_struct_decl(&checked, "Point");
    assert_eq!(decl.name, "Point");
    assert_eq!(decl.fields.len(), 2);
}

#[test]
fn empty_struct_lifts_with_zero_fields() {
    let source = "
        struct Marker
        end
        ";

    let checked = typecheck(&dedent(source));
    let definition = struct_definition(&checked, "Marker");
    assert!(
        definition.fields.is_empty(),
        "empty struct should have zero fields, got {} field(s)",
        definition.fields.len(),
    );
}

#[test]
fn struct_with_mixed_field_types_lifts_each_field_independently() {
    let source = "
        struct Mixed
          flag: Bool
          name: String
          count: Int
        end
        ";

    let checked = typecheck(&dedent(source));
    let definition = struct_definition(&checked, "Mixed");
    let bool_ty = global_leaf(&checked, "Bool");
    let string_ty = global_leaf(&checked, "String");
    let int_ty = global_leaf(&checked, "Int");

    let names: Vec<&str> = definition.fields.iter().map(field_name).collect();
    assert_eq!(names, vec!["flag", "name", "count"]);

    let types: Vec<&ResolvedType> = definition.fields.iter().map(|f| &f.ty).collect();
    assert_eq!(types, vec![&bool_ty, &string_ty, &int_ty]);
}

#[test]
fn nested_struct_field_resolves_to_inner_struct_id() {
    let source = "
        struct Inner
          n: Int
        end

        struct Outer
          inner: Inner
          tag: Bool
        end
        ";

    let checked = typecheck(&dedent(source));
    let outer = struct_definition(&checked, "Outer");
    let inner_leaf = package_leaf(&checked, "Inner");
    let bool_ty = global_leaf(&checked, "Bool");

    assert_eq!(outer.fields.len(), 2);
    assert_eq!(outer.fields[0].ty, inner_leaf);
    assert_eq!(outer.fields[1].ty, bool_ty);
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

#[test]
fn struct_construction_resolves_to_struct_leaf() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          Point{x: 1, y: 2}
        end
        ";

    let checked = typecheck(&dedent(source));
    let trailing = body_trailing_expr(&checked, "main");
    let expected = package_leaf(&checked, "Point");
    assert_eq!(trailing.resolution, expected);

    let ExprKind::StructConstruction { type_path, fields } = &trailing.kind else {
        panic!("expected StructConstruction, got {:?}", trailing.kind);
    };
    assert_eq!(type_path, &["Point".to_string()]);
    assert_eq!(fields.len(), 2);
    let int = global_leaf(&checked, "Int");
    for field in fields {
        assert_eq!(field.value.resolution, int);
    }
}

#[test]
fn struct_construction_accepts_out_of_order_fields() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          Point{y: 2, x: 1}
        end
        ";

    let checked = typecheck(&dedent(source));
    let trailing = body_trailing_expr(&checked, "main");
    let expected = package_leaf(&checked, "Point");
    assert_eq!(trailing.resolution, expected);
}

// ---------------------------------------------------------------------------
// Field access
// ---------------------------------------------------------------------------

#[test]
fn field_access_resolves_to_declared_field_type() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          Point{x: 1, y: 2}.x
        end
        ";

    let checked = typecheck(&dedent(source));
    let trailing = body_trailing_expr(&checked, "main");
    let int = global_leaf(&checked, "Int");
    assert_eq!(trailing.resolution, int);

    let ExprKind::FieldAccess { receiver, field } = &trailing.kind else {
        panic!("expected FieldAccess, got {:?}", trailing.kind);
    };
    assert_eq!(field, "x");
    let point = package_leaf(&checked, "Point");
    assert_eq!(receiver.resolution, point);
}

#[test]
fn nested_field_access_resolves_through_inner_struct() {
    let source = "
        struct Inner
          n: Int
        end

        struct Outer
          inner: Inner
        end

        fn main
          Outer{inner: Inner{n: 5}}.inner.n
        end
        ";

    let checked = typecheck(&dedent(source));
    let trailing = body_trailing_expr(&checked, "main");
    let int = global_leaf(&checked, "Int");
    assert_eq!(trailing.resolution, int);
}

// ---------------------------------------------------------------------------
// Negative — feature gaps
// ---------------------------------------------------------------------------

#[test]
fn annotated_struct_diagnoses_feature_gap() {
    let source = "
        @derive
        struct Point
          x: Int
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("annotations on struct items")),
        "expected struct-annotation gap diagnostic, got {messages:?}",
    );
}

#[test]
fn default_field_value_diagnoses_feature_gap() {
    let source = "
        struct Point
          x: Int = 0
          y: Int
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("default field values")),
        "expected default-field-value gap diagnostic, got {messages:?}",
    );
}

// ---------------------------------------------------------------------------
// Negative — construction validation
// ---------------------------------------------------------------------------

#[test]
fn unknown_struct_type_diagnoses() {
    let source = "
        fn main
          Missing{x: 1}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("does not recognize the struct type `Missing`")),
        "expected unknown-struct diagnostic, got {messages:?}",
    );
}

#[test]
fn primitive_struct_construction_diagnoses() {
    let source = "
        fn main
          Int{x: 1}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("cannot construct primitive type `Global.Int`")),
        "expected primitive-construction diagnostic, got {messages:?}",
    );
}

#[test]
fn unknown_field_in_construction_diagnoses() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          Point{x: 1, y: 2, z: 3}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`TestApp.Point` has no field `z`")),
        "expected unknown-field diagnostic, got {messages:?}",
    );
}

#[test]
fn missing_field_in_construction_diagnoses() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          Point{x: 1}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("missing field `y`") && m.contains("`TestApp.Point`")),
        "expected missing-field diagnostic, got {messages:?}",
    );
}

#[test]
fn duplicate_field_in_construction_diagnoses() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          Point{x: 1, x: 2, y: 3}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("field `x`") && m.contains("initialized twice")),
        "expected duplicate-field diagnostic, got {messages:?}",
    );
}

#[test]
fn wrong_field_type_in_construction_diagnoses() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          Point{x: true, y: 2}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("field `x`")
            && m.contains("expects `Int`")
            && m.contains("got `Bool`")),
        "expected wrong-field-type diagnostic, got {messages:?}",
    );
}

// ---------------------------------------------------------------------------
// Negative — field access validation
// ---------------------------------------------------------------------------

#[test]
fn unknown_field_on_struct_access_diagnoses() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          Point{x: 1, y: 2}.z
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`TestApp.Point` has no field `z`")),
        "expected unknown-field-access diagnostic, got {messages:?}",
    );
}

#[test]
fn field_access_on_non_struct_diagnoses() {
    let source = "
        fn main
          1.x
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("field access requires a struct receiver")),
        "expected non-struct-receiver diagnostic, got {messages:?}",
    );
}

fn field_name(field: &ResolvedStructField) -> &str {
    field.name.as_str()
}

// ---------------------------------------------------------------------------
// Static methods (inline + impl-block forms)
// ---------------------------------------------------------------------------

fn method_signature<'a>(
    checked: &'a CheckedProgram,
    type_name: &str,
    method_name: &str,
) -> &'a FunctionSignature {
    let identifier = Identifier::new(
        PACKAGE,
        vec![type_name.to_string(), method_name.to_string()],
    );
    let (_, entry) = checked
        .registry
        .lookup(&identifier)
        .unwrap_or_else(|| panic!("`{identifier}` not registered"));
    match &entry.kind {
        GlobalKind::Function(Some(signature)) => signature,
        other => panic!("expected lifted Function for `{identifier}`, got {other:?}"),
    }
}

#[test]
fn inline_static_method_registers_under_qualified_identifier() {
    let source = "
        struct Point
          x: Int
          y: Int

          fn origin -> Point
            Point{x: 0, y: 0}
          end
        end
        ";

    let checked = typecheck(&dedent(source));
    let signature = method_signature(&checked, "Point", "origin");
    assert!(signature.params.is_empty());
    assert_eq!(signature.return_type, package_leaf(&checked, "Point"));
}

#[test]
fn impl_block_static_method_registers_under_qualified_identifier() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        impl Point
          fn origin -> Point
            Point{x: 0, y: 0}
          end
        end
        ";

    let checked = typecheck(&dedent(source));
    let signature = method_signature(&checked, "Point", "origin");
    assert!(signature.params.is_empty());
    assert_eq!(signature.return_type, package_leaf(&checked, "Point"));
}

#[test]
fn impl_block_before_struct_in_file_still_registers_methods() {
    // Two-pass collect: pass 1 registers `struct Point`, pass 2
    // registers methods inside `impl Point`. Source order between
    // the two declarations doesn't matter — matches the language
    // rule "all top-level decls visible everywhere".
    let source = "
        impl Point
          fn origin -> Point
            Point{x: 0, y: 0}
          end
        end

        struct Point
          x: Int
          y: Int
        end
        ";

    let checked = typecheck(&dedent(source));
    let signature = method_signature(&checked, "Point", "origin");
    assert_eq!(signature.return_type, package_leaf(&checked, "Point"));
}

#[test]
fn inline_and_impl_static_method_with_same_name_collide() {
    let source = "
        struct Point
          x: Int

          fn origin -> Int
            0
          end
        end

        impl Point
          fn origin -> Int
            1
          end
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`TestApp.Point.origin`") && m.contains("already defined")),
        "expected duplicate-method diagnostic, got {messages:?}",
    );
}

#[test]
fn static_method_call_resolves_to_method_return_type() {
    let source = "
        struct Point
          x: Int
          y: Int

          fn origin -> Point
            Point{x: 0, y: 0}
          end
        end

        fn main
          Point.origin()
        end
        ";

    let checked = typecheck(&dedent(source));
    let trailing = body_trailing_expr(&checked, "main");
    assert_eq!(trailing.resolution, package_leaf(&checked, "Point"));

    let ExprKind::MethodCall {
        receiver, method, ..
    } = &trailing.kind
    else {
        panic!("expected MethodCall, got {:?}", trailing.kind);
    };
    assert_eq!(method, "origin");
    assert_eq!(receiver.resolution, package_leaf(&checked, "Point"));
}

#[test]
fn static_method_with_args_validates_arity_and_types() {
    // Bodies don't reference parameter names — the locals slice
    // hasn't landed yet. The signature still pins arity/types so
    // the call site goes through the validation we want to test.
    let source = "
        struct Point
          x: Int

          fn make(initial: Int, _scale: Int) -> Int
            42
          end
        end

        fn main
          Point.make(1, 2)
        end
        ";

    let checked = typecheck(&dedent(source));
    let trailing = body_trailing_expr(&checked, "main");
    assert_eq!(trailing.resolution, global_leaf(&checked, "Int"));
}

#[test]
fn static_method_call_arity_mismatch_diagnoses() {
    let source = "
        struct Point
          x: Int

          fn make(initial: Int, _scale: Int) -> Int
            42
          end
        end

        fn main
          Point.make(1)
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`TestApp.Point.make`") && m.contains("expects 2 arguments")),
        "expected arity-mismatch diagnostic, got {messages:?}",
    );
}

#[test]
fn static_method_call_arg_type_mismatch_diagnoses() {
    let source = "
        struct Point
          x: Int

          fn make(value: Int) -> Int
            42
          end
        end

        fn main
          Point.make(true)
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("argument `value`")
            && m.contains("`TestApp.Point.make`")
            && m.contains("expects `Int`")
            && m.contains("got `Bool`")),
        "expected arg-type-mismatch diagnostic, got {messages:?}",
    );
}

#[test]
fn nonexistent_static_method_diagnoses() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          Point.frobnicate()
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`TestApp.Point` has no static method `frobnicate`")),
        "expected nonexistent-method diagnostic, got {messages:?}",
    );
}

#[test]
fn instance_method_in_struct_body_lifts_with_dispatch_instance() {
    let source = "
        struct Point
          x: Int

          fn distance(self) -> Int
            self.x
          end
        end
        ";

    let checked = typecheck(&dedent(source));
    let signature = lookup_function_signature(&checked, "TestApp", &["Point", "distance"]);
    assert_eq!(signature.dispatch, Dispatch::Instance);
    assert_eq!(signature.params.len(), 1, "self lifts as a real param");
    assert_eq!(signature.params[0].name, "self");
    let receiver_id = lookup_struct_id(&checked, "TestApp", "Point");
    assert_eq!(
        signature.params[0].ty.resolution,
        Resolution::Global(receiver_id),
        "self carries the enclosing struct's identifier",
    );
}

#[test]
fn instance_method_in_impl_block_lifts_with_dispatch_instance() {
    let source = "
        struct Point
          x: Int
        end

        impl Point
          fn distance(self) -> Int
            self.x
          end
        end
        ";

    let checked = typecheck(&dedent(source));
    let signature = lookup_function_signature(&checked, "TestApp", &["Point", "distance"]);
    assert_eq!(signature.dispatch, Dispatch::Instance);
    assert_eq!(signature.params.len(), 1);
    let receiver_id = lookup_struct_id(&checked, "TestApp", "Point");
    assert_eq!(
        signature.params[0].ty.resolution,
        Resolution::Global(receiver_id),
    );
}

#[test]
fn static_method_self_return_type_resolves_to_enclosing_struct() {
    let source = "
        struct Point
          x: Int

          fn origin -> Self
            Point{x: 0}
          end
        end
        ";

    let program = typecheck(&dedent(source));
    let identifier = Identifier::new("TestApp", vec!["Point".to_string(), "origin".to_string()]);
    let (_, entry) = program
        .registry
        .lookup(&identifier)
        .expect("Point.origin registered");
    let GlobalKind::Function(Some(sig)) = &entry.kind else {
        panic!("Point.origin should have a lifted signature");
    };
    let Resolution::Global(point_id) = sig.return_type.resolution else {
        panic!(
            "expected `Self` to resolve to Global(Point), got {:?}",
            sig.return_type
        );
    };
    let point_identifier = Identifier::new("TestApp", vec!["Point".to_string()]);
    let (expected_id, _) = program
        .registry
        .lookup(&point_identifier)
        .expect("Point registered");
    assert_eq!(
        point_id, expected_id,
        "`Self` must alias the enclosing struct id"
    );
    assert!(
        sig.return_type.type_args.is_empty(),
        "non-generic Point's `Self` carries no type args, got {:?}",
        sig.return_type.type_args,
    );
}

#[test]
fn instance_method_call_resolves_to_method_return_type() {
    let source = "
        struct Point
          x: Int
          y: Int

          fn first(self) -> Int
            self.x
          end
        end

        fn main
          Point{x: 1, y: 2}.first()
        end
        ";

    let checked = typecheck(&dedent(source));
    let trailing = body_trailing_expr(&checked, "main");
    assert_eq!(
        trailing.resolution,
        global_leaf(&checked, "Int"),
        "trailing instance call's resolution should match the method's return type",
    );
    let ExprKind::MethodCall { method, .. } = &trailing.kind else {
        panic!("expected MethodCall, got {:?}", trailing.kind);
    };
    assert_eq!(method, "first");
}

#[test]
fn instance_method_called_as_static_diagnoses() {
    let source = "
        struct Point
          x: Int

          fn distance(self) -> Int
            self.x
          end
        end

        fn main
          Point.distance()
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("instance method") && m.contains("static")),
        "expected instance-as-static diagnostic, got {messages:?}",
    );
}

#[test]
fn static_method_called_on_instance_diagnoses() {
    let source = "
        struct Point
          x: Int
          y: Int

          fn origin -> Point
            Point{x: 0, y: 0}
          end
        end

        fn main
          Point{x: 1, y: 2}.origin()
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("static method") && m.contains("on a value")),
        "expected static-on-instance diagnostic, got {messages:?}",
    );
}

#[test]
fn instance_method_with_args_validates_explicit_args() {
    let source = "
        struct Point
          x: Int

          fn shifted(self, by: Int) -> Int
            self.x
          end
        end

        fn main
          Point{x: 1}.shifted()
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("expects 1 argument")
            || m.contains("expected 1 argument")
            || (m.contains("`shifted`") && m.contains("argument"))),
        "expected arity diagnostic for explicit args (self excluded), got {messages:?}",
    );
}

#[test]
fn impl_with_extra_trait_args_diagnoses_arity() {
    let source = "
        protocol Greeter
          fn greet(self) -> Int
        end

        struct Point
          x: Int
        end

        impl Greeter<String> for Point
          fn greet(self) -> Int
            0
          end
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("expects 0 type arguments, got 1")),
        "expected protocol arity diagnostic, got {messages:?}",
    );
}

#[test]
fn generic_protocol_impl_with_concrete_args_succeeds() {
    // Names picked to not collide with future stdlib auto-import
    // (no `Eq`, `Ord`, etc.).
    let source = "
        protocol Match<T>
          fn matches(self, other: T) -> Bool
        end

        struct User
          id: Int
        end

        impl Match<String> for User
          fn matches(self, other: String) -> Bool
            true
          end
        end
        ";

    let program = typecheck(&dedent(source));
    let identifier = Identifier::new("TestApp", vec!["User".to_string(), "matches".to_string()]);
    let (_, entry) = program
        .registry
        .lookup(&identifier)
        .expect("User.matches registered");
    let GlobalKind::Function(Some(sig)) = &entry.kind else {
        panic!("User.matches should have a lifted signature");
    };
    let other_ty = &sig.params[1].ty;
    let Resolution::Global(string_id) = other_ty.resolution else {
        panic!("expected `other: String`, got {:?}", other_ty);
    };
    let (expected_string_id, _) = program
        .registry
        .lookup(&Identifier::new("Global", vec!["String".to_string()]))
        .expect("String registered");
    assert_eq!(string_id, expected_string_id);
}

#[test]
fn generic_protocol_impl_with_wrong_concrete_arg_diagnoses() {
    let source = "
        protocol Match<T>
          fn matches(self, other: T) -> Bool
        end

        struct User
          id: Int
        end

        impl Match<String> for User
          fn matches(self, other: Int) -> Bool
            true
          end
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("does not match protocol")
                && m.contains("Global.String")
                && m.contains("Global.Int")),
        "expected substituted-type mismatch diagnostic, got {messages:?}",
    );
}

#[test]
fn generic_target_impl_anchors_self_at_receiver_id() {
    // `impl Render for Bag<T>` — the impl block's free type-param
    // `T` aliases the receiver struct's slot-0 anchor. This pins:
    //   - `self: Bag<T>` resolves to `Bag<TypeParam(Bag, 0)>`,
    //     identical to an inline `fn render(self)` on `struct Bag<T>`,
    //   - methods register at `[Bag, render]` regardless of the
    //     `<T>` decoration on the target head,
    //   - call-site inference substitutes via the receiver's
    //     type-args alone (no separate impl scope).
    //
    // Names are picked deliberately to not collide with anything
    // a future stdlib auto-import would bring in.
    let source = "
        protocol Render
          fn render(self) -> Int
        end

        struct Bag<T>
          item: T
        end

        impl Render for Bag<T>
          fn render(self) -> Int
            0
          end
        end
        ";

    let program = typecheck(&dedent(source));
    let method_identifier = Identifier::new(PACKAGE, vec!["Bag".to_string(), "render".to_string()]);
    let (_, entry) = program
        .registry
        .lookup(&method_identifier)
        .expect("Bag.render registered");
    let GlobalKind::Function(Some(sig)) = &entry.kind else {
        panic!("Bag.render should have a lifted signature");
    };
    let self_ty = &sig.params[0].ty;
    let Resolution::Global(_) = self_ty.resolution else {
        panic!("expected self: Bag<...>, got {:?}", self_ty);
    };
    assert_eq!(self_ty.type_args.len(), 1, "Bag has one type-arg slot");
    let Resolution::TypeParam { owner, .. } = self_ty.type_args[0].resolution else {
        panic!(
            "expected self's `T` to be a TypeParam, got {:?}",
            self_ty.type_args[0]
        );
    };
    let bag_identifier = Identifier::new(PACKAGE, vec!["Bag".to_string()]);
    let (bag_id, _) = program
        .registry
        .lookup(&bag_identifier)
        .expect("Bag registered");
    assert_eq!(
        owner, bag_id,
        "trait-impl method `T` must alias the struct's slot-0 anchor",
    );
}

#[test]
fn generic_target_impl_method_call_resolves_concrete_receiver() {
    // Regression guard: calling a trait-impl method on a generic
    // receiver must dispatch through the impl-anchored `self` type
    // and produce a fully-resolved call site (no `TypeParam` leaks
    // through the substituted return type into the AST). Slice 2.8
    // anchored `T` at the impl entry rather than the receiver
    // struct, so the inference step has to substitute via the impl
    // owner — not just the struct owner.
    let source = "
        protocol Render
          fn render(self) -> Int
        end

        struct Bag<T>
          item: T
        end

        impl Render for Bag<T>
          fn render(self) -> Int
            0
          end
        end

        fn use_bag()
          Bag{item: 1}.render()
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn generic_target_impl_method_returning_free_param_substitutes() {
    // Hard case: the impl method returns the impl's *own* free
    // type-param `T`. Slice 2.8 anchored `T` at the impl entry, so
    // the call-site inference step must substitute the impl owner
    // when computing the return type — otherwise the result leaks a
    // `TypeParam(impl_id, 0)` into the call site's resolution and
    // seal panics.
    let source = "
        protocol Pick<T>
          fn pick(self) -> T
        end

        struct Bag<T>
          item: T
        end

        impl Pick<T> for Bag<T>
          fn pick(self) -> T
            self.item
          end
        end

        fn use_bag() -> Int
          Bag{item: 1}.pick()
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn trait_impl_on_concrete_target_args_dispatches_only_for_matching_receiver() {
    // "Extend"-style domain check: `impl Render for Bag<Int>`
    // adds `render` to `Bag<Int>` only. Calls on `Bag<Int>` succeed;
    // calls on `Bag<String>` fail at the receiver-type check rather
    // than dispatching incorrectly.
    let source = "
        protocol Render
          fn render(self) -> Int
        end

        struct Bag<T>
          item: T
        end

        impl Render for Bag<Int>
          fn render(self) -> Int
            0
          end
        end

        fn use_int_bag() -> Int
          Bag{item: 1}.render()
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn trait_impl_on_concrete_target_args_diagnoses_mismatched_receiver() {
    let source = "
        protocol Render
          fn render(self) -> Int
        end

        struct Bag<T>
          item: T
        end

        impl Render for Bag<Int>
          fn render(self) -> Int
            0
          end
        end

        fn use_string_bag()
          Bag{item: \"x\"}.render()
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("no method `render`") && m.contains("Bag")),
        "expected 'no method on receiver' diagnostic, got {messages:?}",
    );
}

#[test]
fn general_and_specialized_trait_impls_collide_on_shared_method_name() {
    // Both impls want `[Bag, render]` — collision detected at
    // method registration. This is the cornerstone of the
    // "extend"-style design: any two impl blocks that define the
    // same method name on the same type head are a hard error,
    // regardless of whether the targets are general or specialized.
    let source = "
        protocol Render
          fn render(self) -> Int
        end

        struct Bag<T>
          item: T
        end

        impl Render for Bag<T>
          fn render(self) -> Int
            0
          end
        end

        impl Render for Bag<Int>
          fn render(self) -> Int
            1
          end
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("already defined") && m.contains("Bag.render")),
        "expected duplicate-method/impl diagnostic, got {messages:?}",
    );
}

#[test]
fn impl_with_type_alias_member_diagnoses_feature_gap() {
    let source = "
        struct Point
          x: Int
        end

        impl Point
          type Coord = Int
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`type` aliases inside `impl` blocks")),
        "expected impl-typealias gap diagnostic, got {messages:?}",
    );
}

#[test]
fn impl_on_unknown_type_diagnoses() {
    let source = "
        impl Vector
          fn zero -> Int
            0
          end
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("cannot extend unknown type `Vector`")),
        "expected impl-unknown-type diagnostic, got {messages:?}",
    );
}

// ---------------------------------------------------------------------------
// Generics — definition, lift, construction inference
// ---------------------------------------------------------------------------

#[test]
fn generic_struct_lifts_with_type_params_and_typeparam_field_resolutions() {
    let source = "
        struct Pair<T, U>
          a: T
          b: U
        end
        ";

    let checked = typecheck(&dedent(source));
    let pair_id = lookup_struct_id(&checked, PACKAGE, "Pair");
    let entry = checked
        .registry
        .get(pair_id)
        .expect("registered Pair entry");
    assert_eq!(entry.type_params, vec!["T".to_string(), "U".to_string()]);
    let definition = struct_definition(&checked, "Pair");
    assert_eq!(definition.fields.len(), 2);
    assert!(matches!(
        definition.fields[0].ty.resolution,
        Resolution::TypeParam { owner, .. } if owner == pair_id,
    ));
    assert!(matches!(
        definition.fields[1].ty.resolution,
        Resolution::TypeParam { owner, .. } if owner == pair_id,
    ));
    assert_ne!(
        definition.fields[0].ty.resolution, definition.fields[1].ty.resolution,
        "T and U must mint distinct TypeParam handles",
    );
}

#[test]
fn generic_struct_construction_infers_type_args_from_field_values() {
    let source = "
        struct Pair<T, U>
          a: T
          b: U
        end

        fn main
          Pair{a: 1, b: \"x\"}
        end
        ";

    let checked = typecheck(&dedent(source));
    let pair_id = lookup_struct_id(&checked, PACKAGE, "Pair");
    let int = global_leaf(&checked, "Int");
    let string = global_leaf(&checked, "String");
    let expr = body_trailing_expr(&checked, "main");
    assert_eq!(expr.resolution.resolution, Resolution::Global(pair_id));
    assert_eq!(expr.resolution.type_args, vec![int, string]);
}

#[test]
fn generic_struct_construction_with_conflicting_inferences_diagnoses() {
    let source = "
        struct Pair<T, U>
          a: T
          b: T
        end

        fn main
          Pair{a: 1, b: \"x\"}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("type parameter `T` of `TestApp.Pair` cannot be both")),
        "expected type-param conflict diagnostic, got {messages:?}",
    );
}

#[test]
fn generic_struct_phantom_type_param_diagnoses() {
    let source = "
        struct Phantom<T>
          marker: Int
        end

        fn main
          Phantom{marker: 1}
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("cannot infer type parameter `T`")),
        "expected Phantom-guard diagnostic, got {messages:?}",
    );
}

#[test]
fn generic_struct_nested_in_generic_struct_resolves_through_typeparam_args() {
    let source = "
        struct Pair<T, U>
          a: T
          b: U
        end

        struct Box<V>
          inner: Pair<V, Int>
        end
        ";

    let checked = typecheck(&dedent(source));
    let box_definition = struct_definition(&checked, "Box");
    let box_id = lookup_struct_id(&checked, PACKAGE, "Box");
    let pair_id = lookup_struct_id(&checked, PACKAGE, "Pair");
    let int = global_leaf(&checked, "Int");

    let inner = &box_definition.fields[0].ty;
    assert_eq!(inner.resolution, Resolution::Global(pair_id));
    assert_eq!(inner.type_args.len(), 2);
    assert!(matches!(
        inner.type_args[0].resolution,
        Resolution::TypeParam { owner, .. } if owner == box_id,
    ));
    assert_eq!(inner.type_args[1], int);
}
