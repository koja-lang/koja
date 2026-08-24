use koja_ast::ast::{AnnotationKind, Item};
use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_typecheck::{FunctionOrigin, GlobalKind};

mod common;

use common::{PACKAGE, assert_file_fails_with, test_file, typecheck_file};

#[test]
fn registers_top_level_overloads_by_exact_arity() {
    let checked = typecheck_file(&dedent(
        r#"
        fn pick(x: Int) -> Int
          x
        end

        fn pick(x: Int, y: Int) -> Int
          x + y
        end
        "#,
    ));
    let identifier = Identifier::new(PACKAGE, vec!["pick".to_string()]);

    let (one, _) = checked.registry.lookup_function(&identifier, 1).unwrap();
    let (two, _) = checked.registry.lookup_function(&identifier, 2).unwrap();
    assert_ne!(one, two);
}

#[test]
fn rejects_same_arity_overloads_regardless_of_types() {
    assert_file_fails_with(
        &dedent(
            r#"
            fn pick(x: Int) -> Int
              x
            end

            fn pick<T>(x: T) -> T
              x
            end
            "#,
        ),
        &["already defined"],
    );
}

#[test]
fn expands_each_default_arity_and_resolves_calls() {
    let checked = typecheck_file(&dedent(
        r#"
        fn sum(a: Int, b: Int = 2, c: Int = 3) -> Int
          a + b + c
        end

        fn use -> Int
          sum(1) + sum(1, 4) + sum(1, 4, 5)
        end
        "#,
    ));
    let identifier = Identifier::new(PACKAGE, vec!["sum".to_string()]);

    for arity in 1..=3 {
        let (_, entry) = checked
            .registry
            .lookup_function(&identifier, arity)
            .unwrap();
        if arity < 3 {
            let GlobalKind::Function(definition) = &entry.kind else {
                panic!("expected function entry");
            };
            assert_eq!(
                definition.origin,
                FunctionOrigin::DefaultAdapter { canonical_arity: 3 }
            );
        }
    }

    let adapters = test_file(&checked)
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function)
                if matches!(
                    function.origin,
                    FunctionOrigin::DefaultAdapter { canonical_arity: 3 }
                ) =>
            {
                Some(function.params.len())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(adapters, vec![1, 2]);
}

#[test]
fn method_arity_counts_self() {
    let checked = typecheck_file(&dedent(
        r#"
        struct Box
          value: Int

          fn get(self) -> Int
            self.value
          end

          fn get(self, fallback: Int) -> Int
            fallback
          end
        end
        "#,
    ));
    let identifier = Identifier::new(PACKAGE, vec!["Box".to_string(), "get".to_string()]);
    assert!(checked.registry.lookup_function(&identifier, 1).is_some());
    assert!(checked.registry.lookup_function(&identifier, 2).is_some());
}

#[test]
fn rejects_overlapping_default_ranges() {
    assert_file_fails_with(
        &dedent(
            r#"
            fn pick(x: Int) -> Int
              x
            end

            fn pick(x: Int, y: Int = 1) -> Int
              x + y
            end
            "#,
        ),
        &["already defined"],
    );
}

#[test]
fn rejects_parameter_and_self_references_in_defaults() {
    assert_file_fails_with(
        &dedent(
            r#"
            fn bad(a: Int, b: Int = a) -> Int
              b
            end

            struct Box
              value: Int

              fn bad(self, x: Int = self.value) -> Int
                x
              end
            end
            "#,
        ),
        &["cannot reference parameter `a`", "cannot reference `self`"],
    );
}

#[test]
fn allows_nested_closure_to_shadow_parameter_name() {
    typecheck_file(&dedent(
        r#"
        fn apply(f: fn (Int) -> Int, x: Int = 1) -> Int
          f(x)
        end

        fn wrapper(x: Int, f: fn (Int) -> Int = fn (x: Int) -> Int x end) -> Int
          apply(f)
        end
        "#,
    ));
}

#[test]
fn generic_and_extern_defaults_register_adapters() {
    let checked = typecheck_file(&dedent(
        r#"
        fn keep<T>(value: T, fallback: Option<T> = Option.None) -> T
          value
        end

        struct FFI
          @extern "C" @link "c"
          fn abs(value: Int32 = 1) -> Int32
        end
        "#,
    ));
    let extern_identifier = Identifier::new(PACKAGE, vec!["FFI".to_string(), "abs".to_string()]);
    let (_, adapter) = checked
        .registry
        .lookup_function(&extern_identifier, 0)
        .unwrap();
    let GlobalKind::Function(definition) = &adapter.kind else {
        panic!("expected function entry");
    };
    assert_eq!(
        definition.origin,
        FunctionOrigin::DefaultAdapter { canonical_arity: 1 }
    );
}

#[test]
fn protocol_defaults_provide_lower_arities() {
    typecheck_file(&dedent(
        r#"
        protocol Greeter
          fn greet(self, suffix: String = "!") -> String
            suffix
          end
        end

        struct Cat: Greeter
        end

        fn use(cat: Cat) -> String
          cat.greet()
        end
        "#,
    ));
}

#[test]
fn protocol_implementations_cannot_repeat_defaults() {
    assert_file_fails_with(
        &dedent(
            r#"
            protocol Greeter
              fn greet(self, suffix: String = "!") -> String
                suffix
              end
            end

            struct Cat
            end

            impl Greeter for Cat
              fn greet(self, suffix: String = "?") -> String
                suffix
              end
            end
            "#,
        ),
        &["Defaults belong to the protocol"],
    );
}

#[test]
fn protocol_overloads_match_implementations_by_name_and_arity() {
    typecheck_file(&dedent(
        r#"
        protocol Convert
          fn convert(self) -> Int
          fn convert(self, base: Int) -> Int
        end

        struct Number: Convert
          value: Int

          fn convert(self) -> Int
            self.value
          end

          fn convert(self, base: Int) -> Int
            self.value + base
          end
        end
        "#,
    ));
}

#[test]
fn protocol_default_ranges_cannot_overlap_explicit_arities() {
    assert_file_fails_with(
        &dedent(
            r#"
            protocol Convert
              fn convert(self) -> Int

              fn convert(self, base: Int = 10) -> Int
                base
              end
            end
            "#,
        ),
        &["with arity 1 is already defined"],
    );
}

#[test]
fn adapters_do_not_inherit_test_or_doc_annotations() {
    let checked = typecheck_file(&dedent(
        r#"
        struct Example
          @test "uses a default"
          @doc "Checks a default."
          fn check(value: Int = 1) -> Result<Bool, String>
            Result.Ok(value == 1)
          end
        end
        "#,
    ));
    let declaration = common::find_struct_decl(&checked, "Example");
    let test_count = declaration
        .functions
        .iter()
        .flat_map(|function| &function.annotations)
        .filter(|annotation| annotation.name == "test")
        .count();
    let doc_count = declaration
        .functions
        .iter()
        .flat_map(|function| &function.annotations)
        .filter(|annotation| matches!(annotation.kind(), AnnotationKind::Doc(_)))
        .count();
    assert_eq!(test_count, 1);
    assert_eq!(doc_count, 1);
}

#[test]
fn match_bindings_shadow_parameter_names_in_defaults() {
    typecheck_file(&dedent(
        r#"
        fn wrapper(
          value: Int,
          callback: fn (Int) -> Int = fn (input: Int) -> Int
            match input
              value -> value
            end
          end
        ) -> Int
          callback(value)
        end
        "#,
    ));
}
