use koja_ast::ast::{
    BinOp, Expr, ExprKind, ImplBlock, ImplMember, Item, Literal, Statement, TypeExpr,
};
use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_parser::ParseMode;
use koja_typecheck::CheckedProgram;

mod common;

use common::{
    PACKAGE, check_multi_file, global_id, registry_id, typecheck_script as typecheck,
    typecheck_script_fail,
};

fn equality_impl<'a>(checked: &'a CheckedProgram, type_name: &str) -> Option<&'a ImplBlock> {
    let package = checked
        .packages
        .iter()
        .find(|package| package.package == PACKAGE)
        .expect("test package should be present");
    package
        .files
        .iter()
        .flat_map(|file| &file.items)
        .find_map(|item| match item {
            Item::Impl(block)
                if type_expr_head(&block.trait_expr) == Some("Equality")
                    && type_expr_head(&block.target) == Some(type_name) =>
            {
                Some(block)
            }
            _ => None,
        })
}

fn equality_body<'a>(checked: &'a CheckedProgram, type_name: &str) -> &'a Expr {
    if let Some(block) = equality_impl(checked, type_name) {
        for member in &block.members {
            let ImplMember::Function(function) = member else {
                continue;
            };
            if function.name != "equals?" {
                continue;
            }
            let body = function.body.as_ref().expect("eq should have a body");
            let Statement::Expr(expression) = &body[0] else {
                panic!("eq body should contain one expression");
            };
            return expression;
        }
    }
    panic!("no synthesized `Equality for {type_name}` impl found");
}

/// Whether the test package's `type_name` records an `Equality`
/// conformance and still registers `equals?/2`.
fn derives_equality(checked: &CheckedProgram, type_name: &str) -> bool {
    let target_id = registry_id(checked, PACKAGE, &[type_name]);
    let equality_id = global_id(checked, "Equality");
    let method = Identifier::new(PACKAGE, vec![type_name.to_string(), "equals?".to_string()]);
    let conforms = checked.registry.conforms_any(target_id, equality_id);
    let registered = checked.registry.lookup_function(&method, 2).is_some();
    assert_eq!(
        conforms, registered,
        "conformance fact and `equals?` registration must agree for `{type_name}`"
    );
    conforms
}

fn type_expr_head(type_expr: &TypeExpr) -> Option<&str> {
    match type_expr {
        TypeExpr::Generic { path, .. } | TypeExpr::Named { path, .. } => {
            path.last().map(String::as_str)
        }
        _ => None,
    }
}

fn count_eq_calls(expression: &Expr) -> usize {
    match &expression.kind {
        ExprKind::Binary { left, right, .. } => count_eq_calls(left) + count_eq_calls(right),
        ExprKind::MethodCall { method, .. } if method == "equals?" => 1,
        _ => 0,
    }
}

#[test]
fn enum_synthesizes_one_outer_arm_per_variant() {
    let source = "
        enum Shape
          Point
          Tagged(Int)
          Labeled{name: String}
        end

        1
        ";

    let checked = typecheck(&dedent(source));
    let ExprKind::Match { arms, .. } = &equality_body(&checked, "Shape").kind else {
        panic!("Shape.eq should use an outer match");
    };
    assert_eq!(arms.len(), 3);
}

#[test]
fn generic_struct_uses_universal_equality_dispatch() {
    let source = "
        struct Pair<A, B>
          first: A
          second: B
        end

        1
        ";

    let checked = typecheck(&dedent(source));
    assert_eq!(count_eq_calls(equality_body(&checked, "Pair")), 2);
}

#[test]
fn manual_impl_in_sibling_file_suppresses_synthesis() {
    let checked = check_multi_file(
        &[
            (
                "custom.koja",
                "
                struct Custom
                  value: Int
                end
                ",
            ),
            (
                "equality.koja",
                "
                impl Equality for Custom
                  fn equals?(self, other: Custom) -> Bool
                    self.value == other.value
                  end
                end
                ",
            ),
        ],
        ParseMode::File,
    )
    .expect("manual Equality impl should typecheck");

    let count = checked
        .packages
        .iter()
        .flat_map(|package| &package.files)
        .flat_map(|file| &file.items)
        .filter(|item| {
            let Item::Impl(block) = item else {
                return false;
            };
            type_expr_head(&block.trait_expr) == Some("Equality")
                && type_expr_head(&block.target) == Some("Custom")
        })
        .count();
    assert_eq!(count, 1);
}

#[test]
fn opaque_fields_collapse_to_true() {
    let source = "
        struct Handle
          pointer: CPtr<Int32>
        end

        1
        ";

    let checked = typecheck(&dedent(source));
    assert!(matches!(
        equality_body(&checked, "Handle").kind,
        ExprKind::Literal {
            value: Literal::Bool(true)
        }
    ));
}

#[test]
fn struct_fields_synthesize_conjunction() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        1
        ";

    let checked = typecheck(&dedent(source));
    let body = equality_body(&checked, "Point");
    assert!(matches!(body.kind, ExprKind::Binary { op: BinOp::And, .. }));
    assert_eq!(count_eq_calls(body), 2);
}

#[test]
fn generic_derive_bounds_every_type_param_by_equality() {
    let source = "
        struct Pair<A, B>
          first: A
          second: B
        end

        1
        ";

    let checked = typecheck(&dedent(source));
    let block = equality_impl(&checked, "Pair").expect("Pair should derive Equality");
    let bounds: Vec<(&str, Vec<Option<&str>>)> = block
        .target_bounds
        .iter()
        .map(|param| {
            (
                param.name.as_str(),
                param.bounds.iter().map(type_expr_head).collect(),
            )
        })
        .collect();
    assert_eq!(
        bounds,
        vec![("A", vec![Some("Equality")]), ("B", vec![Some("Equality")])]
    );
}

#[test]
fn function_field_retracts_the_derived_impl() {
    let source = "
        struct Holder
          hook: fn (Int) -> Int
          n: Int
        end

        1
        ";

    let checked = typecheck(&dedent(source));
    assert!(!derives_equality(&checked, "Holder"));
    assert!(equality_impl(&checked, "Holder").is_none());
}

#[test]
fn function_inside_generic_field_retracts_the_derived_impl() {
    let source = "
        struct Holder
          hook: Option<fn (Int) -> Int>
        end

        struct Registry
          hooks: List<fn (Int) -> Int>
        end

        1
        ";

    let checked = typecheck(&dedent(source));
    assert!(!derives_equality(&checked, "Holder"));
    assert!(!derives_equality(&checked, "Registry"));
}

#[test]
fn retraction_propagates_to_containing_types() {
    let source = "
        struct Inner
          f: fn (Int) -> Int
        end

        struct Outer
          inner: Inner
          n: Int
        end

        1
        ";

    let checked = typecheck(&dedent(source));
    assert!(!derives_equality(&checked, "Inner"));
    assert!(!derives_equality(&checked, "Outer"));
}

#[test]
fn function_payload_retracts_the_derived_enum_impl() {
    let source = "
        enum Event
          Hook(fn (Int) -> Int)
          Nothing
        end

        1
        ";

    let checked = typecheck(&dedent(source));
    assert!(!derives_equality(&checked, "Event"));
}

#[test]
fn conforming_fields_keep_the_derived_impl() {
    let source = "
        struct Point
          x: Int
          tags: List<String>
          label: Option<String>
          pair: (Int, String)
        end

        1
        ";

    let checked = typecheck(&dedent(source));
    assert!(derives_equality(&checked, "Point"));
}

#[test]
fn equality_on_retracted_type_names_the_blocking_field() {
    let source = "
        struct Holder
          hook: Option<fn (Int) -> Int>
        end

        fn same(a: Holder, b: Holder) -> Bool
          a == b
        end

        1
        ";

    let failure = typecheck_script_fail(&dedent(source));
    let diagnostic = failure
        .diagnostics
        .iter()
        .find(|d| d.message.contains("does not implement `Equality`"))
        .expect("== on a retracted type should be rejected");
    assert_eq!(
        diagnostic.hint.as_deref(),
        Some(
            "field `hook` has type `Option<fn (Int) -> Int>`, which does not implement `Equality`"
        ),
    );
}

#[test]
fn equality_on_generic_instantiation_names_the_payload() {
    let source = "
        fn double(x: Int) -> Int
          x * 2
        end

        a: Option<fn (Int) -> Int> = Option.Some(&double/1)
        b: Option<fn (Int) -> Int> = Option.Some(&double/1)
        a == b
        ";

    let failure = typecheck_script_fail(&dedent(source));
    let diagnostic = failure
        .diagnostics
        .iter()
        .find(|d| d.message.contains("does not implement `Equality`"))
        .expect("== on Option<fn> should be rejected");
    assert_eq!(
        diagnostic.hint.as_deref(),
        Some(
            "the payload of `Some` has type `fn (Int) -> Int`, which does not implement `Equality`"
        ),
    );
}

#[test]
fn conditional_protocol_method_is_unavailable_on_unmet_instantiation() {
    let source = "
        fn double(x: Int) -> Int
          x * 2
        end

        a: List<fn (Int) -> Int> = [&double/1]
        b: List<fn (Int) -> Int> = [&double/1]
        a.equals?(b)
        ";

    common::assert_script_fails_with(
        source,
        &["`List<fn (Int) -> Int>` does not implement `Equality`, so `equals?` is unavailable"],
    );
}
