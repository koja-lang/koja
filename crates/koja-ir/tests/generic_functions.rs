//! IR-lowering coverage for the generics slice's function arm
//! (`src/generics/monomorphize.rs::monomorphize_function`).
//!
//! Pins the body-substitute-and-relower contract:
//!
//! - Generic function templates never appear in [`IRPackage`]; only
//!   monomorphized concrete functions land there, mangled with their
//!   args.
//! - Distinct args at the same call site mint distinct functions.
//! - Repeated calls with the same args dedupe to one function.
//! - Methods on a generic struct/enum get enqueued automatically when
//!   the receiver type is monomorphized; the resulting symbol uses the
//!   struct's mangled prefix derived through [`IRSymbol::derived`].
//! - Calls inside a substituted body (a generic function calling
//!   another generic function) cascade through the worklist.
//!
//! Mangled-name shape for top-level generic functions mirrors generic
//! types — `<root>_$<arg>.<arg>$`. Methods on a generic type read as
//! `<type-mangled>.<method>` (no separate args list, since methods
//! inherit the type's params).

use koja_ast::util::dedent;
use koja_ir::{IRFunction, IRType};

mod common;

use common::lower_script_source;

fn collect_script_function_names(script: &koja_ir::IRScript) -> Vec<String> {
    let mut names: Vec<String> = script
        .packages
        .iter()
        .flat_map(|p| p.functions.keys())
        .map(|sym| sym.mangled().to_string())
        .collect();
    names.sort();
    names
}

fn function<'a>(script: &'a koja_ir::IRScript, mangled: &str) -> &'a IRFunction {
    script
        .function(mangled)
        .unwrap_or_else(|| panic!("expected function `{mangled}` in script"))
}

#[test]
fn identity_function_monomorphizes_at_each_concrete_arg() {
    let source = "
        fn id<T>(x: T) -> T
          x
        end

        id(1)
        id(\"hello\")
        ";

    let script = lower_script_source(&dedent(source));

    let names = collect_script_function_names(&script);
    assert!(
        !names.iter().any(|n| n == "TestApp.id"),
        "generic template `TestApp.id` must not appear in IRPackage.functions",
    );
    assert!(names.contains(&"TestApp.id_$Int64$".to_string()));
    assert!(names.contains(&"TestApp.id_$String$".to_string()));

    let int_id = function(&script, "TestApp.id_$Int64$");
    assert_eq!(int_id.return_type, IRType::Int64);
    assert_eq!(int_id.params.len(), 1);
    assert_eq!(int_id.params[0].ty, IRType::Int64);

    let string_id = function(&script, "TestApp.id_$String$");
    assert_eq!(string_id.return_type, IRType::String);
    assert_eq!(string_id.params.len(), 1);
    assert_eq!(string_id.params[0].ty, IRType::String);
}

#[test]
fn idempotent_calls_dedupe_to_one_function_per_arg_set() {
    let source = "
        fn id<T>(x: T) -> T
          x
        end

        id(1)
        id(2)
        id(3)
        ";

    let script = lower_script_source(&dedent(source));
    let id_decls: Vec<_> = collect_script_function_names(&script)
        .into_iter()
        .filter(|n| n.starts_with("TestApp.id"))
        .collect();
    assert_eq!(id_decls, vec!["TestApp.id_$Int64$".to_string()]);
}

#[test]
fn multi_param_function_mangles_args_in_declaration_order() {
    let source = "
        fn pick<T, U>(a: T, b: U) -> T
          a
        end

        pick(1, \"x\")
        pick(\"y\", 2)
        ";

    let script = lower_script_source(&dedent(source));
    let mut pick_decls: Vec<_> = collect_script_function_names(&script)
        .into_iter()
        .filter(|n| n.starts_with("TestApp.pick"))
        .collect();
    pick_decls.sort();
    assert_eq!(
        pick_decls,
        vec![
            "TestApp.pick_$Int64.String$".to_string(),
            "TestApp.pick_$String.Int64$".to_string(),
        ],
    );

    let first = function(&script, "TestApp.pick_$Int64.String$");
    assert_eq!(first.params[0].ty, IRType::Int64);
    assert_eq!(first.params[1].ty, IRType::String);
    assert_eq!(first.return_type, IRType::Int64);
}

#[test]
fn generic_function_calling_another_generic_cascades_through_worklist() {
    let source = "
        fn id<T>(x: T) -> T
          x
        end

        fn passthrough<U>(y: U) -> U
          id(y)
        end

        passthrough(7)
        ";

    let script = lower_script_source(&dedent(source));
    let names = collect_script_function_names(&script);
    assert!(names.contains(&"TestApp.passthrough_$Int64$".to_string()));
    assert!(
        names.contains(&"TestApp.id_$Int64$".to_string()),
        "calling `passthrough(7)` must transitively monomorphize `id<Int>`; got {names:?}",
    );
}

#[test]
fn generic_function_called_with_user_struct_includes_struct_in_mangle() {
    let source = "
        struct Inner
          n: Int
        end

        fn id<T>(x: T) -> T
          x
        end

        id(Inner{n: 1})
        ";

    let script = lower_script_source(&dedent(source));
    let id_decls: Vec<_> = collect_script_function_names(&script)
        .into_iter()
        .filter(|n| n.starts_with("TestApp.id"))
        .collect();
    assert_eq!(id_decls, vec!["TestApp.id_$TestApp.Inner$".to_string()]);
}

#[test]
fn generic_function_called_with_generic_arg_yields_nested_mangle() {
    let source = "
        struct Box<T>
          value: T
        end

        fn id<T>(x: T) -> T
          x
        end

        id(Box{value: 1})
        ";

    let script = lower_script_source(&dedent(source));
    let names = collect_script_function_names(&script);
    let id_decls: Vec<_> = names
        .iter()
        .filter(|n| n.starts_with("TestApp.id"))
        .cloned()
        .collect();
    assert_eq!(
        id_decls,
        vec!["TestApp.id_$TestApp.Box_$Int64$$".to_string()]
    );
}

#[test]
fn method_on_generic_struct_monomorphizes_with_struct_mangled_prefix() {
    let source = "
        struct Pair<T, U>
          a: T
          b: U

          fn first(self) -> T
            self.a
          end
        end

        p = Pair{a: 1, b: \"x\"}
        0
        ";

    let script = lower_script_source(&dedent(source));
    let names = collect_script_function_names(&script);
    assert!(
        names.contains(&"TestApp.Pair_$Int64.String$.first".to_string()),
        "method `Pair<Int, String>.first` should monomorphize when `Pair<Int, String>` is \
         constructed; got {names:?}",
    );
    assert!(
        !names.iter().any(|n| n == "TestApp.Pair.first"),
        "generic template `TestApp.Pair.first` must not appear in IRPackage.functions",
    );

    let mangled = "TestApp.Pair_$Int64.String$.first";
    let method = function(&script, mangled);
    assert_eq!(method.return_type, IRType::Int64);
}

#[test]
fn method_on_generic_struct_for_distinct_args_mints_distinct_methods() {
    let source = "
        struct Pair<T, U>
          a: T
          b: U

          fn first(self) -> T
            self.a
          end
        end

        p = Pair{a: 1, b: \"x\"}
        q = Pair{a: \"y\", b: 2}
        0
        ";

    let script = lower_script_source(&dedent(source));
    let mut firsts: Vec<_> = collect_script_function_names(&script)
        .into_iter()
        .filter(|n| n.starts_with("TestApp.Pair_") && n.ends_with(".first"))
        .collect();
    firsts.sort();
    assert_eq!(
        firsts,
        vec![
            "TestApp.Pair_$Int64.String$.first".to_string(),
            "TestApp.Pair_$String.Int64$.first".to_string(),
        ],
    );
}
