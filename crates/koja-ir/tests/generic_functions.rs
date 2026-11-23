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
//! - A widening inside a substituted body targets the concrete union.
//!
//! Mangled-name shape for top-level generic functions mirrors generic
//! types: `<root>_$<arg>.<arg>$`. Methods on a generic type read as
//! `<type-mangled>.<method>` (no separate args list, since methods
//! inherit the type's params).

use koja_ir::{IRFunction, IRInstruction, IRType};

mod common;

use common::{all_instructions, lower_script_source, mangled_function, script_function_names};

#[test]
fn identity_function_monomorphizes_at_each_concrete_arg() {
    let source = "
        fn id<T>(x: T) -> T
          x
        end

        id(1)
        id(\"hello\")
        ";

    let script = lower_script_source(source);

    let names = script_function_names(&script);
    assert!(
        !names.iter().any(|n| n == "TestApp.id/1"),
        "generic template `TestApp.id` must not appear in IRPackage.functions",
    );
    assert!(names.contains(&"TestApp.id/1_$Int64$".to_string()));
    assert!(names.contains(&"TestApp.id/1_$String$".to_string()));

    let int_id = mangled_function(&script, "TestApp.id/1_$Int64$");
    assert_eq!(int_id.return_type, IRType::Int64);
    assert_eq!(int_id.params.len(), 1);
    assert_eq!(int_id.params[0].ty, IRType::Int64);

    let string_id = mangled_function(&script, "TestApp.id/1_$String$");
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

    let script = lower_script_source(source);
    let id_decls: Vec<_> = script_function_names(&script)
        .into_iter()
        .filter(|n| n.starts_with("TestApp.id"))
        .collect();
    assert_eq!(id_decls, vec!["TestApp.id/1_$Int64$".to_string()]);
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

    let script = lower_script_source(source);
    let mut pick_decls: Vec<_> = script_function_names(&script)
        .into_iter()
        .filter(|n| n.starts_with("TestApp.pick"))
        .collect();
    pick_decls.sort();
    assert_eq!(
        pick_decls,
        vec![
            "TestApp.pick/2_$Int64.String$".to_string(),
            "TestApp.pick/2_$String.Int64$".to_string(),
        ],
    );

    let first = mangled_function(&script, "TestApp.pick/2_$Int64.String$");
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

    let script = lower_script_source(source);
    let names = script_function_names(&script);
    assert!(names.contains(&"TestApp.passthrough/1_$Int64$".to_string()));
    assert!(
        names.contains(&"TestApp.id/1_$Int64$".to_string()),
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

    let script = lower_script_source(source);
    let id_decls: Vec<_> = script_function_names(&script)
        .into_iter()
        .filter(|n| n.starts_with("TestApp.id"))
        .collect();
    assert_eq!(id_decls, vec!["TestApp.id/1_$TestApp.Inner$".to_string()]);
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

    let script = lower_script_source(source);
    let names = script_function_names(&script);
    let id_decls: Vec<_> = names
        .iter()
        .filter(|n| n.starts_with("TestApp.id"))
        .cloned()
        .collect();
    assert_eq!(
        id_decls,
        vec!["TestApp.id/1_$TestApp.Box_$Int64$$".to_string()]
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

    let script = lower_script_source(source);
    let names = script_function_names(&script);
    assert!(
        names.contains(&"TestApp.Pair_$Int64.String$.first/1".to_string()),
        "method `Pair<Int, String>.first` should monomorphize when `Pair<Int, String>` is \
         constructed; got {names:?}",
    );
    assert!(
        !names.iter().any(|n| n == "TestApp.Pair.first/1"),
        "generic template `TestApp.Pair.first` must not appear in IRPackage.functions",
    );

    let method = mangled_function(&script, "TestApp.Pair_$Int64.String$.first/1");
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

    let script = lower_script_source(source);
    let mut firsts: Vec<_> = script_function_names(&script)
        .into_iter()
        .filter(|n| n.starts_with("TestApp.Pair_") && n.ends_with(".first/1"))
        .collect();
    firsts.sort();
    assert_eq!(
        firsts,
        vec![
            "TestApp.Pair_$Int64.String$.first/1".to_string(),
            "TestApp.Pair_$String.Int64$.first/1".to_string(),
        ],
    );
}

#[test]
fn widening_into_a_union_with_a_type_parameter_lowers_to_union_wrap() {
    // The typecheck records the widening of `[item]` with the target
    // `Int | List<T>`. Monomorphization must substitute that target,
    // or the lowering meets `T` and panics.
    let source = "
        fn wrap<T>(item: T) -> Int | List<T>
          [item]
        end

        wrap(\"a\")
        ";

    let script = lower_script_source(source);

    let wrap = mangled_function(&script, "TestApp.wrap/1_$String$");
    assert!(
        matches!(wrap.return_type, IRType::Union(_)),
        "expected `wrap<String>` to return a union, got {:?}",
        wrap.return_type,
    );
    assert_eq!(
        union_wrap_count(wrap),
        1,
        "expected one UnionWrap for the list that widens into the return type",
    );
}

#[test]
fn widening_of_a_type_parameter_value_lowers_to_union_wrap_on_each_return() {
    // The union is `Int | T`, so the type parameter is a member. The
    // early return widens `item` and the tail widens `0`.
    let source = "
        fn pick<T>(item: T, flag: Bool) -> Int | T
          if flag
            return item
          end

          0
        end

        pick(\"b\", true)
        ";

    let script = lower_script_source(source);

    let pick = mangled_function(&script, "TestApp.pick/2_$String$");
    assert!(
        matches!(pick.return_type, IRType::Union(_)),
        "expected `pick<String>` to return a union, got {:?}",
        pick.return_type,
    );
    assert_eq!(
        union_wrap_count(pick),
        2,
        "expected one UnionWrap for `item` and one for `0`",
    );
}

#[test]
fn widening_into_a_typed_local_of_a_generic_body_lowers_to_union_wrap() {
    // The widening target is the annotation of the local `held`, not
    // the return type.
    let source = "
        fn hold<T>(item: T) -> Int
          held: Int | List<T> = [item]

          match held
            n: Int -> n
            items: List<T> -> items.length()
          end
        end

        hold(\"c\")
        ";

    let script = lower_script_source(source);

    let hold = mangled_function(&script, "TestApp.hold/1_$String$");
    assert_eq!(
        union_wrap_count(hold),
        1,
        "expected one UnionWrap for the list that widens into `held`",
    );
}

#[test]
fn widening_in_a_method_of_a_generic_struct_lowers_to_union_wrap() {
    // The type parameter comes from the struct, not from the method.
    let source = "
        struct Holder<T>
          value: T

          fn either(self, flag: Bool) -> Int | T
            if flag
              return self.value
            end

            0
          end
        end

        holder: Holder<String> = Holder{value: \"d\"}
        holder.either(true)
        ";

    let script = lower_script_source(source);

    let either = mangled_function(&script, "TestApp.Holder_$String$.either/2");
    assert!(
        matches!(either.return_type, IRType::Union(_)),
        "expected `Holder<String>.either` to return a union, got {:?}",
        either.return_type,
    );
    assert_eq!(
        union_wrap_count(either),
        2,
        "expected one UnionWrap for `self.value` and one for `0`",
    );
}

fn union_wrap_count(function: &IRFunction) -> usize {
    all_instructions(&function.blocks)
        .filter(|i| matches!(i, IRInstruction::UnionWrap { .. }))
        .count()
}
