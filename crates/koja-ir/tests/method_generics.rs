//! IR-lowering coverage for method-level type parameters
//! (`fn map<U>(self, f: fn(T) -> U) -> Box<U>` etc.).
//!
//! Methods on a generic type that *only* use the type's params
//! (`fn first(self) -> T` on `Pair<T, U>`) are pinned by
//! `generic_functions.rs`. This file pins the orthogonal case: a
//! method that introduces its own type parameters on top of (or
//! independent of) the receiver's. Symbol shape is
//! `<receiver-mangled>.<method>_$<method-args>$`, minted by
//! [`koja_ir::mangling::mangled_method_name`] at both the
//! call site and during monomorphization so the two agree.

use koja_ast::util::dedent;
use koja_ir::IRScript;

mod common;

use common::lower_script_source;

fn collect_function_names(script: &IRScript) -> Vec<String> {
    let mut names: Vec<String> = script
        .packages
        .iter()
        .flat_map(|p| p.functions.keys())
        .map(|sym| sym.mangled().to_string())
        .collect();
    names.sort();
    names
}

#[test]
fn method_with_own_type_param_monomorphizes_with_receiver_and_method_args() {
    let source = "
        struct Box<T>
          value: T

          fn map<U>(self, f: fn (T) -> U) -> Box<U>
            Box{value: f(self.value)}
          end
        end

        fn to_string(x: Int) -> String
          \"x\"
        end

        b = Box{value: 1}
        b.map(to_string)
        ";

    let script = lower_script_source(&dedent(source));
    let names = collect_function_names(&script);

    let mangled = "TestApp.Box_$Int64$.map_$String$";
    assert!(
        names.contains(&mangled.to_string()),
        "expected monomorphized method `{mangled}`; got {names:?}",
    );
    assert!(
        !names.iter().any(|n| n == "TestApp.Box.map"),
        "unspecialized template `TestApp.Box.map` must not appear in IRPackage.functions",
    );

    let method = script
        .function(mangled)
        .unwrap_or_else(|| panic!("missing method `{mangled}`"));
    assert_eq!(method.symbol.mangled(), mangled);
}

#[test]
fn distinct_method_args_mint_distinct_specializations() {
    let source = "
        struct Box<T>
          value: T

          fn map<U>(self, f: fn (T) -> U) -> Box<U>
            Box{value: f(self.value)}
          end
        end

        fn to_string(x: Int) -> String
          \"x\"
        end

        fn double(x: Int) -> Int
          x + x
        end

        b = Box{value: 1}
        b.map(to_string)
        b.map(double)
        ";

    let script = lower_script_source(&dedent(source));
    let names = collect_function_names(&script);

    assert!(
        names.contains(&"TestApp.Box_$Int64$.map_$String$".to_string()),
        "expected `Box<Int>::map<String>` specialization; got {names:?}",
    );
    assert!(
        names.contains(&"TestApp.Box_$Int64$.map_$Int64$".to_string()),
        "expected `Box<Int>::map<Int>` specialization; got {names:?}",
    );
}

#[test]
fn method_with_no_type_params_is_unaffected_by_method_args_lookup() {
    let source = "
        struct Pair<T, U>
          a: T
          b: U

          fn first(self) -> T
            self.a
          end
        end

        p = Pair{a: 1, b: \"x\"}
        p.first()
        ";

    let script = lower_script_source(&dedent(source));
    let names = collect_function_names(&script);

    let mangled = "TestApp.Pair_$Int64.String$.first";
    assert!(
        names.contains(&mangled.to_string()),
        "expected struct-level-only method specialization `{mangled}`; got {names:?}",
    );
}
