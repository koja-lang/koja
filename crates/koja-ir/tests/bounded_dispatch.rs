//! IR-lowering coverage for bounded dispatch. `fn show<T: Greeter>`
//! calls `value.greet()`, and monomorphization rewrites the receiver
//! to the concrete struct so `lower_method_call` resolves the impl
//! method like an inherent one. Pins that each instantiation lowers
//! cleanly, that distinct instantiations mint distinct `show`s, and
//! that the impl method lands in the package.

mod common;

use common::{lower_script_source, script_function_names};

#[test]
fn bounded_dispatch_monomorphizes_show_at_concrete_arg() {
    // `show<T: Greeter>(value: T) -> String { value.greet() }` is
    // monomorphized at `T = Point` when called with a `Point`. The
    // resulting concrete `show_$Point$` body lowers a call to
    // `Point.greet`, the trait-impl method registered by the
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

    let script = lower_script_source(source);
    let names = script_function_names(&script);
    assert!(
        names.contains(&"TestApp.show/1_$TestApp.Point$".to_string()),
        "expected mono'd `show_$TestApp.Point$`, got {names:?}",
    );
    assert!(
        !names.iter().any(|n| n == "TestApp.show/1"),
        "generic template `TestApp.show` must not appear in IRPackage.functions",
    );
    assert!(
        names.contains(&"TestApp.Point.greet/1".to_string()),
        "expected concrete impl method `TestApp.Point.greet`, got {names:?}",
    );
}

#[test]
fn bounded_dispatch_distinct_concrete_args_mint_distinct_show_decls() {
    // Two structs implementing the same protocol yield two distinct
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

    let script = lower_script_source(source);
    let mut shows: Vec<_> = script_function_names(&script)
        .into_iter()
        .filter(|n| n.starts_with("TestApp.show"))
        .collect();
    shows.sort();
    assert_eq!(
        shows,
        vec![
            "TestApp.show/1_$TestApp.Point$".to_string(),
            "TestApp.show/1_$TestApp.Tag$".to_string(),
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

    let script = lower_script_source(source);
    let names = script_function_names(&script);
    assert!(
        names.contains(&"TestApp.show/1_$TestApp.Bag_$Int64$$".to_string()),
        "expected mono'd `show_$Bag<Int>$`, got {names:?}",
    );
    assert!(
        names.contains(&"TestApp.Bag_$Int64$.greet/1".to_string()),
        "expected mono'd `Bag<Int>.greet` impl method, got {names:?}",
    );
}

#[test]
fn parameterized_bound_dispatch_monomorphizes_protocol_arguments() {
    let source = "
        protocol Source<T>
          fn first(self) -> T
        end

        struct IntSource
          value: Int
        end

        impl Source<Int> for IntSource
          fn first(self) -> Int
            self.value
          end
        end

        fn read<T, E: Source<T>>(source: E, fallback: T) -> T
          source.first()
        end

        read(IntSource{value: 1}, 0)
        0
        ";

    let script = lower_script_source(source);
    let names = script_function_names(&script);
    assert!(
        names
            .iter()
            .any(|name| name.starts_with("TestApp.read/2_$")),
        "expected parameterized `read` specialization, got {names:?}",
    );
    assert!(names.contains(&"TestApp.IntSource.first/1".to_string()));
}
