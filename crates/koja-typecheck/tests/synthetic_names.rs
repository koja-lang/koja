//! Compiler-synthesized names carry synthetic spans.
//!
//! The resolve pass rewrites the AST in place and builds `Name`s and
//! hidden local reads that copy a user node's positions. Reference
//! lookups read every `Name` span, so those copies must be synthetic.
//! The user's own names and the span of the wrapping expression stay
//! real.

use koja_ast::ast::{
    ExprKind, FunctionOrigin, Item, MatchArm, Pattern, Statement, StringPart, TypeExpr,
};
use koja_ast::util::dedent;

mod common;

use common::{
    script_body, test_file, trailing_expr, typecheck_file, typecheck_script as typecheck,
};

#[test]
fn struct_equality_method_name_is_synthetic() {
    let source = "
        struct Point
          x: Int
        end

        a = Point{x: 1}
        b = Point{x: 1}
        a == b
        ";
    let checked = typecheck(&dedent(source));
    let expr = trailing_expr(&checked);
    let ExprKind::MethodCall { method, .. } = &expr.kind else {
        panic!(
            "expected `==` on a struct to rewrite to a MethodCall, got {:?}",
            expr.kind
        );
    };
    assert_eq!(method, "equals?");
    assert!(
        method.span.synthetic,
        "synthesized method name is synthetic"
    );
    assert!(!expr.span.synthetic, "the user's `a == b` span stays real");
}

#[test]
fn interpolation_format_method_name_is_synthetic() {
    let source = "
        x = 1
        \"#{x}\"
        ";
    let checked = typecheck(&dedent(source));
    let string = trailing_expr(&checked);
    let ExprKind::String { parts, .. } = &string.kind else {
        panic!("expected a string expression, got {:?}", string.kind);
    };
    let Some(StringPart::Interpolation { expr, .. }) = parts
        .iter()
        .find(|part| matches!(part, StringPart::Interpolation { .. }))
    else {
        panic!("expected one interpolation part, got {parts:?}");
    };
    let ExprKind::MethodCall {
        method, receiver, ..
    } = &expr.kind
    else {
        panic!(
            "expected the interpolation to wrap in `.format()`, got {:?}",
            expr.kind
        );
    };
    assert_eq!(method, "format");
    assert!(
        method.span.synthetic,
        "synthesized method name is synthetic"
    );
    assert!(
        !expr.span.synthetic,
        "the wrapping call keeps the user's span"
    );
    assert!(!receiver.span.synthetic, "the user's `x` stays real");
}

#[test]
fn for_loop_hidden_names_are_synthetic_and_user_pattern_is_not() {
    let source = "
        for x in [1, 2]
          x
        end
        ";
    let checked = typecheck(&dedent(source));
    let body = script_body(&checked);
    assert_eq!(
        body.len(),
        3,
        "source and cursor assignments, then the loop"
    );
    let Statement::Assignment { target, .. } = &body[0] else {
        panic!("expected the hidden source assignment, got {:?}", body[0]);
    };
    assert!(
        target.segments[0].span.synthetic,
        "hidden local name is synthetic"
    );
    let Statement::Expr(loop_expr) = &body[2] else {
        panic!("expected the desugared loop, got {:?}", body[2]);
    };
    assert!(!loop_expr.span.synthetic, "the loop keeps the user's span");
    let ExprKind::Loop { body: loop_body } = &loop_expr.kind else {
        panic!("expected ExprKind::Loop, got {:?}", loop_expr.kind);
    };
    let Some(Statement::Expr(match_expr)) = loop_body.first() else {
        panic!("expected the loop body to start with a match, got {loop_body:?}");
    };
    let ExprKind::Match { subject, arms } = &match_expr.kind else {
        panic!("expected ExprKind::Match, got {:?}", match_expr.kind);
    };
    let ExprKind::MethodCall {
        method, receiver, ..
    } = &subject.kind
    else {
        panic!(
            "expected a `.next(...)` call as the subject, got {:?}",
            subject.kind
        );
    };
    assert!(
        method.span.synthetic,
        "synthesized `next` name is synthetic"
    );
    assert!(receiver.span.synthetic, "hidden local read is synthetic");
    let Some(MatchArm { pattern, .. }) = arms.first() else {
        panic!("expected a Some arm");
    };
    let Pattern::EnumTuple {
        type_path,
        variant,
        elements,
        ..
    } = pattern
    else {
        panic!("expected the Some arm to resolve to an EnumTuple, got {pattern:?}");
    };
    assert!(type_path[0].span.synthetic, "`Option` segment is synthetic");
    assert!(variant.span.synthetic, "`Some` variant is synthetic");
    let [Pattern::Tuple { elements: pair, .. }] = elements.as_slice() else {
        panic!("expected one tuple element, got {elements:?}");
    };
    let [
        Pattern::Binding { name: user, .. },
        Pattern::Binding { name: rest, .. },
    ] = pair.as_slice()
    else {
        panic!("expected the user binding and the hidden rest binding, got {pair:?}");
    };
    assert_eq!(user, "x");
    assert!(!user.span.synthetic, "the user's `x` stays real");
    assert!(rest.span.synthetic, "hidden rest binding is synthetic");
}

#[test]
fn rescue_hidden_binder_is_synthetic_and_user_binder_is_not() {
    let source = "
        enum MyError
          Nope
        end

        fn parse(s: String) -> Int ! MyError
          fail MyError.Nope
        end

        parse(\"1\") rescue e -> 0
        ";
    let checked = typecheck(&dedent(source));
    let expr = trailing_expr(&checked);
    let ExprKind::Match { arms, .. } = &expr.kind else {
        panic!(
            "expected `rescue` to rewrite to a match, got {:?}",
            expr.kind
        );
    };
    let [ok_arm, err_arm] = arms.as_slice() else {
        panic!("expected an Ok arm and an Err arm, got {arms:?}");
    };
    let Pattern::EnumTuple {
        variant, elements, ..
    } = &ok_arm.pattern
    else {
        panic!("expected an EnumTuple Ok pattern, got {:?}", ok_arm.pattern);
    };
    assert!(variant.span.synthetic, "`Ok` variant is synthetic");
    let [Pattern::Binding { name: hidden, .. }] = elements.as_slice() else {
        panic!("expected one hidden binder, got {elements:?}");
    };
    assert!(hidden.span.synthetic, "hidden Ok binder is synthetic");
    let Pattern::EnumTuple { elements, .. } = &err_arm.pattern else {
        panic!(
            "expected an EnumTuple Err pattern, got {:?}",
            err_arm.pattern
        );
    };
    let [Pattern::Binding { name: user, .. }] = elements.as_slice() else {
        panic!("expected the user's binder, got {elements:?}");
    };
    assert_eq!(user, "e");
    assert!(!user.span.synthetic, "the user's `e` stays real");
}

#[test]
fn test_block_function_name_is_synthetic() {
    let source = "
        test \"adds\"
          assert 1 == 1
        end
        ";
    let checked = typecheck_file(&dedent(source));
    let function = test_file(&checked)
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function) if function.origin == FunctionOrigin::Test => Some(function),
            _ => None,
        })
        .expect("test block desugared into a function");
    assert!(
        function.name.span.synthetic,
        "synthesized name is synthetic"
    );
    assert!(
        !function.span.synthetic,
        "the function keeps the block's span"
    );
    let Some(TypeExpr::Named { path, .. }) = &function.error_type else {
        panic!(
            "expected a named error channel, got {:?}",
            function.error_type
        );
    };
    assert!(
        path.iter().all(|segment| segment.span.synthetic),
        "`Test.Failure` segments are synthetic"
    );
}
