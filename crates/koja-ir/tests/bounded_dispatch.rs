//! IR-lowering coverage for the bounded-dispatch slice (Slice 2.10).
//!
//! `fn show<T: Greeter>(value: T) -> String` calls `value.greet()`
//! inside its body. At typecheck the receiver stays
//! `TypeParam(show, 0)`; at IR mono the substitute walker rewrites
//! it to the concrete struct, and `lower_method_call` resolves
//! `[receiver_struct, "greet"]` against the registry to find the
//! concrete impl method. This file pins:
//!
//! - The mono'd `show` function exists and lowers without
//!   surfacing diagnostics (i.e. the substituted body sealed cleanly
//!   and the method call resolved to a concrete callee).
//! - Distinct concrete instantiations mint distinct mono'd `show`s.
//! - The concrete impl method (e.g. `Point.greet`) is lowered into
//!   the IR package alongside the mono'd `show`.
//!
//! No `protocol_impls` table walk lives at the call site — the
//! receiver-substitute path through [`super::lower::expr::lower_method_call`]
//! reuses the same lookup an inherent method goes through, so this
//! test mirrors the inherent-method-on-concrete-type shape rather
//! than asserting on any IR-side rewrite hook.

use koja_ast::util::dedent;

mod common;

use common::lower_script_source;

fn collect_function_names(script: &koja_ir::IRScript) -> Vec<String> {
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
fn bounded_dispatch_monomorphizes_show_at_concrete_arg() {
    // `show<T: Greeter>(value: T) -> String { value.greet() }` is
    // monomorphized at `T = Point` when called with a `Point`. The
    // resulting concrete `show_$Point$` body lowers a call to
    // `Point.greet` — the trait-impl method registered by the
    // `impl Greeter for Point` block.
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Point
          x: Int
        end

        impl Greeter for Point
          fn greet(self) -> String
            \"Point\"
          end
        end

        fn show<T: Greeter>(value: T) -> String
          value.greet()
        end

        show(Point{x: 1})
        0
        ";

    let script = lower_script_source(&dedent(source));
    let names = collect_function_names(&script);
    assert!(
        names.contains(&"TestApp.show_$TestApp.Point$".to_string()),
        "expected mono'd `show_$TestApp.Point$`, got {names:?}",
    );
    assert!(
        !names.iter().any(|n| n == "TestApp.show"),
        "generic template `TestApp.show` must not appear in IRPackage.functions",
    );
    assert!(
        names.contains(&"TestApp.Point.greet".to_string()),
        "expected concrete impl method `TestApp.Point.greet`, got {names:?}",
    );
}

#[test]
fn bounded_dispatch_distinct_concrete_args_mint_distinct_show_decls() {
    // Two structs implementing the same protocol -> two distinct
    // mono'd `show` functions, one per receiver type. Confirms that
    // each call's receiver substitution flows independently through
    // the worklist.
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Point
          x: Int
        end

        impl Greeter for Point
          fn greet(self) -> String
            \"Point\"
          end
        end

        struct Tag
          label: String
        end

        impl Greeter for Tag
          fn greet(self) -> String
            self.label
          end
        end

        fn show<T: Greeter>(value: T) -> String
          value.greet()
        end

        show(Point{x: 1})
        show(Tag{label: \"hi\"})
        0
        ";

    let script = lower_script_source(&dedent(source));
    let mut shows: Vec<_> = collect_function_names(&script)
        .into_iter()
        .filter(|n| n.starts_with("TestApp.show"))
        .collect();
    shows.sort();
    assert_eq!(
        shows,
        vec![
            "TestApp.show_$TestApp.Point$".to_string(),
            "TestApp.show_$TestApp.Tag$".to_string(),
        ],
    );
}

#[test]
fn bounded_dispatch_generic_struct_receiver_resolves_through_substitution() {
    // The receiver type is itself generic: `Bag<Int>` implements
    // `Greeter` via `impl Greeter for Bag<T>`. The mono'd `show`
    // calls `Bag_$Int64$.greet`, which the inline-method enqueuing
    // in [`monomorphize::enqueue_member_methods`] (mirrored for
    // impl-block methods through the function index) brings into
    // the package.
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Bag<T>
          item: T
        end

        impl Greeter for Bag<T>
          fn greet(self) -> String
            \"Bag\"
          end
        end

        fn show<T: Greeter>(value: T) -> String
          value.greet()
        end

        show(Bag{item: 1})
        0
        ";

    let script = lower_script_source(&dedent(source));
    let names = collect_function_names(&script);
    assert!(
        names.contains(&"TestApp.show_$TestApp.Bag_$Int64$$".to_string()),
        "expected mono'd `show_$Bag<Int>$`, got {names:?}",
    );
    assert!(
        names.contains(&"TestApp.Bag_$Int64$.greet".to_string()),
        "expected mono'd `Bag<Int>.greet` impl method, got {names:?}",
    );
}
