use koja_ast::ast::{ExprKind, Statement};
use koja_ast::util::dedent;

mod common;

use common::{
    diagnostic_messages, script_body, typecheck_script as typecheck,
    typecheck_script_fail as typecheck_fail,
};

#[test]
fn scalar_literals_rewrite_through_their_protocols() {
    let source = "
        bool_value: JSON.Value = true
        int_value: JSON.Value = -42
        float_value: JSON.Value = -1.5
        string_value: JSON.Value = \"count #{42}\"
        string_value
        ";
    let checked = typecheck(&dedent(source));
    let methods: Vec<&str> = script_body(&checked)
        .iter()
        .filter_map(|statement| match statement {
            Statement::Assignment { value, .. } => match &value.kind {
                ExprKind::MethodCall { method, .. } => Some(method.as_str()),
                other => panic!("expected literal protocol call, got {other:?}"),
            },
            _ => None,
        })
        .collect();

    assert_eq!(
        methods,
        vec!["from_bool", "from_int", "from_float", "from_string"]
    );
}

#[test]
fn conformance_arguments_type_nested_json_literals() {
    let source = r#"
        value: JSON.Value = [
          "items": [1, -2, 3.5, true, "x"],
          "empty_array": [],
          "empty_object": [:],
        ]
        value
        "#;

    typecheck(&dedent(source));
}

#[test]
fn scalar_protocols_do_not_convert_non_literal_values() {
    let source = "
        number = 42
        value: JSON.Value = number
        value
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);

    assert!(
        messages
            .iter()
            .any(|message| message.contains("right-hand side has type `Int`")),
        "expected a contextual type mismatch, got {messages:#?}",
    );
}
