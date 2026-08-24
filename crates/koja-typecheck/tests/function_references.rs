use koja_ast::ast::{Expr, ExprKind, Statement};
use koja_ast::identifier::{AnonymousKind, Identifier, Resolution, ResolvedType};
use koja_ast::util::dedent;
use koja_parser::ParseMode;
use koja_typecheck::{FunctionOrigin, GlobalKind};

mod common;

use common::{
    PACKAGE, assert_file_fails_with, check_packages, function_body, global_leaf, typecheck_file,
    warning_messages,
};

fn assignment_value<'a>(checked: &'a koja_typecheck::CheckedProgram, function: &str) -> &'a Expr {
    let Statement::Assignment { value, .. } = &function_body(checked, function)[0] else {
        panic!("expected the function body to start with an assignment");
    };
    value
}

fn assert_reference(
    checked: &koja_typecheck::CheckedProgram,
    expr: &Expr,
    package: &str,
    path: &[&str],
    arity: usize,
    expected_type: ResolvedType,
) {
    let identifier = Identifier::new(
        package,
        path.iter().map(|part| (*part).to_string()).collect(),
    );
    let (expected_target, _) = checked
        .registry
        .lookup_function(&identifier, arity)
        .expect("expected referenced function in registry");
    let ExprKind::NamedFunctionReference { target, .. } = &expr.kind else {
        panic!("expected a named function reference");
    };
    assert_eq!(*target, Resolution::Global(expected_target));
    assert_eq!(expr.resolution, expected_type);
}

#[test]
fn resolves_same_package_function_reference() {
    let checked = typecheck_file(&dedent(
        r#"
        fn double(value: Int) -> Int
          value * 2
        end

        fn use
          callback = &double/1
        end
        "#,
    ));
    let int = global_leaf(&checked, "Int");
    assert_reference(
        &checked,
        assignment_value(&checked, "use"),
        PACKAGE,
        &["double"],
        1,
        ResolvedType::Anonymous(AnonymousKind::Function {
            params: vec![int.clone()],
            ret: Box::new(int),
        }),
    );
}

#[test]
fn resolves_package_qualified_public_function_reference() {
    let checked = check_packages(
        &[
            (
                "Math",
                "math.koja",
                &dedent(
                    r#"
                    fn double(value: Int) -> Int
                      value * 2
                    end
                    "#,
                ),
            ),
            (
                PACKAGE,
                "app.koja",
                &dedent(
                    r#"
                    fn use
                      callback = &Math.double/1
                    end
                    "#,
                ),
            ),
        ],
        ParseMode::File,
    )
    .expect("package-qualified reference should typecheck");
    let int = global_leaf(&checked, "Int");
    assert_reference(
        &checked,
        assignment_value(&checked, "use"),
        "Math",
        &["double"],
        1,
        ResolvedType::Anonymous(AnonymousKind::Function {
            params: vec![int.clone()],
            ret: Box::new(int),
        }),
    );
}

#[test]
fn resolves_static_and_unbound_instance_references() {
    let checked = typecheck_file(&dedent(
        r#"
        struct Counter
          value: Int

          fn zero -> Int
            0
          end

          fn add(self, amount: Int) -> Int
            self.value + amount
          end
        end

        fn use
          static_fn = &Counter.zero/0
          instance_fn = &Counter.add/2
        end
        "#,
    ));
    let counter = common::package_leaf(&checked, "Counter");
    let int = global_leaf(&checked, "Int");
    let body = function_body(&checked, "use");
    let Statement::Assignment {
        value: static_fn, ..
    } = &body[0]
    else {
        panic!("expected static assignment");
    };
    let Statement::Assignment {
        value: instance_fn, ..
    } = &body[1]
    else {
        panic!("expected instance assignment");
    };
    assert_reference(
        &checked,
        static_fn,
        PACKAGE,
        &["Counter", "zero"],
        0,
        ResolvedType::Anonymous(AnonymousKind::Function {
            params: vec![],
            ret: Box::new(int.clone()),
        }),
    );
    assert_reference(
        &checked,
        instance_fn,
        PACKAGE,
        &["Counter", "add"],
        2,
        ResolvedType::Anonymous(AnonymousKind::Function {
            params: vec![counter, int.clone()],
            ret: Box::new(int),
        }),
    );
}

#[test]
fn resolves_default_adapter_by_its_exact_arity() {
    let checked = typecheck_file(&dedent(
        r#"
        fn greet(name: String, suffix: String = "!") -> String
          name <> suffix
        end

        fn use
          callback = &greet/1
        end
        "#,
    ));
    let identifier = Identifier::new(PACKAGE, vec!["greet".to_string()]);
    let (_, entry) = checked.registry.lookup_function(&identifier, 1).unwrap();
    let GlobalKind::Function(definition) = &entry.kind else {
        panic!("expected function entry");
    };
    assert_eq!(
        definition.origin,
        FunctionOrigin::DefaultAdapter { canonical_arity: 2 }
    );
    assert_reference(
        &checked,
        assignment_value(&checked, "use"),
        PACKAGE,
        &["greet"],
        1,
        ResolvedType::Anonymous(AnonymousKind::Function {
            params: vec![global_leaf(&checked, "String")],
            ret: Box::new(global_leaf(&checked, "String")),
        }),
    );
}

#[test]
fn rejects_missing_arity_and_generic_references() {
    let failure = common::typecheck_file_fail(&dedent(
        r#"
        fn one(value: Int) -> Int
          value
        end

        fn identity<T>(value: T) -> T
          value
        end

        fn use
          missing = &one/2
          generic = &identity/1
        end
        "#,
    ));
    assert!(failure.diagnostics.iter().any(|diagnostic| {
        diagnostic.message.contains("has no arity 2")
            && diagnostic.hint.as_deref() == Some("did you mean `&one/1`?")
    }));
    assert!(failure.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("cannot reference generic function `identity` directly")
    }));
}

#[test]
fn rejects_private_cross_package_reference() {
    let failure = check_packages(
        &[
            (
                "Lib",
                "lib.koja",
                &dedent(
                    r#"
                    priv fn hidden(value: Int) -> Int
                      value
                    end
                    "#,
                ),
            ),
            (
                PACKAGE,
                "app.koja",
                &dedent(
                    r#"
                    fn use
                      callback = &Lib.hidden/1
                    end
                    "#,
                ),
            ),
        ],
        ParseMode::File,
    )
    .expect_err("private cross-package reference should fail");
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("private function `Lib.hidden`"))
    );
}

#[test]
fn package_qualified_wrong_arity_suggests_exact_call() {
    let failure = check_packages(
        &[
            (
                "Math",
                "math.koja",
                &dedent(
                    r#"
                    fn add(left: Int, right: Int) -> Int
                      left + right
                    end
                    "#,
                ),
            ),
            (
                PACKAGE,
                "app.koja",
                &dedent(
                    r#"
                    fn use -> Int
                      Math.add(1)
                    end
                    "#,
                ),
            ),
        ],
        ParseMode::File,
    )
    .expect_err("wrong package function arity should fail");
    let diagnostic = failure
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.message.contains("with arity 1"))
        .expect("expected package function arity diagnostic");
    assert_eq!(
        diagnostic.hint.as_deref(),
        Some("did you mean `Math.add(_, _)`?")
    );
}

#[test]
fn rejects_abstract_and_bound_receiver_references() {
    assert_file_fails_with(
        &dedent(
            r#"
            protocol Render
              fn render(self) -> String
            end

            struct Box
              value: Int

              fn get(self) -> Int
                self.value
              end
            end

            fn use(box: Box)
              abstract_fn = &Render.render/1
              bound_fn = &box.get/1
            end
            "#,
        ),
        &[
            "cannot reference abstract protocol function",
            "cannot bind a local receiver",
        ],
    );
}

#[test]
fn bare_global_function_reads_require_explicit_references() {
    let failure = common::typecheck_file_fail(&dedent(
        r#"
        fn double(value: Int) -> Int
          value * 2
        end

        fn use
          callback = double
        end
        "#,
    ));
    let diagnostic = failure
        .diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic
                .message
                .contains("named function `double` requires an explicit reference")
        })
        .expect("expected bare-function migration diagnostic");
    assert_eq!(
        diagnostic.hint.as_deref(),
        Some("write `&double/1` to select its arity")
    );
    let failure = check_packages(
        &[
            (
                "Math",
                "math.koja",
                &dedent(
                    r#"
                    fn double(value: Int) -> Int
                      value * 2
                    end
                    "#,
                ),
            ),
            (
                PACKAGE,
                "app.koja",
                &dedent(
                    r#"
                    fn use
                      callback = Math.double
                    end
                    "#,
                ),
            ),
        ],
        ParseMode::File,
    )
    .expect_err("qualified bare function read should fail");
    assert!(failure.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("named function `Math.double` requires an explicit reference")
    }));
}

#[test]
fn local_closure_values_and_calls_are_unchanged() {
    typecheck_file(&dedent(
        r#"
        fn use -> Int
          callback = fn (value: Int) -> Int value * 2 end
          copied = callback
          copied(3)
        end
        "#,
    ));
}

#[test]
fn enclosing_type_function_wins_over_package_function() {
    let checked = typecheck_file(&dedent(
        r#"
        fn pick(value: Int) -> Int
          value
        end

        struct Box
          fn pick(value: Int) -> String
            "box"
          end

          fn use
            callback = &pick/1
          end
        end
        "#,
    ));
    let declaration = common::find_struct_decl(&checked, "Box");
    let use_function = declaration
        .functions
        .iter()
        .find(|function| function.name == "use")
        .expect("expected Box.use");
    let Statement::Assignment { value, .. } = &use_function.body.as_ref().unwrap()[0] else {
        panic!("expected reference assignment");
    };
    assert_reference(
        &checked,
        value,
        PACKAGE,
        &["Box", "pick"],
        1,
        ResolvedType::Anonymous(AnonymousKind::Function {
            params: vec![global_leaf(&checked, "Int")],
            ret: Box::new(global_leaf(&checked, "String")),
        }),
    );
}

#[test]
fn named_reference_preserves_deprecation_warning() {
    let checked = typecheck_file(&dedent(
        r#"
        @deprecated "Use `new_value` instead."
        fn old_value -> Int
          1
        end

        fn use
          callback = &old_value/0
        end
        "#,
    ));
    assert!(
        warning_messages(&checked)
            .iter()
            .any(|warning| warning.contains("`old_value` is deprecated"))
    );
}

#[test]
fn reference_to_non_function_declaration_diagnoses() {
    assert_file_fails_with(
        &dedent(
            r#"
            struct Outer
              struct Inner
                x: Int
              end

              fn make() -> Int
                f = &Inner/1
                1
              end
            end
            "#,
        ),
        &["`&Inner/1` does not name a function because `TestApp.Outer.Inner` is a struct"],
    );
}
