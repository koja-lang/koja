//! IR lowering coverage for the `List<T>` family. Pairs against the
//! LLVM emitters in `koja-ir-llvm/src/intrinsics/list.rs` and
//! the eval emitters in `koja-ir-eval/src/intrinsics/list.rs`.
//!
//! Each fixture exercises one method on `Global.List` from a script
//! body that uses the autoimported `list.koja` decl. Assertions
//! confirm:
//!
//! - the `IRType::List(element)` lattice variant surfaces on call
//!   sites and is monomorphized per element type;
//! - method dispatch lands on the matching `IRIntrinsicId::List`
//!   variant on the resolved `IRFunction`;
//! - `Global.List` skips struct-decl emission (it's a primitive
//!   template — backends synthesize the storage layout).
//!
//! The autoimported stdlib already registers `List<T>`, so the
//! script source just calls the intrinsics directly.

use koja_ast::util::dedent;
use koja_ir::{
    FunctionKind, IRFunction, IRInstruction, IRIntrinsicId, IRScript, IRType, ListMethod,
};

mod common;

use common::lower_script_source;

/// Find the `List.<method>` call in the script body and return the
/// resolved `IRFunction`. Mangled names look like
/// `Global.List_$Int64$.<method>` (the receiver's type-args land
/// between the receiver and the method), so matching by both `List_`
/// prefix and `.<method>` suffix is more robust than a single substring.
fn intrinsic_call<'a>(script: &'a IRScript, method_name: &str) -> &'a IRFunction {
    let suffix = format!(".{method_name}");
    let mangled = script
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .find_map(|inst| match inst {
            IRInstruction::Call { callee, .. }
                if callee.mangled().contains(".List_") && callee.mangled().ends_with(&suffix) =>
            {
                Some(callee.mangled().to_string())
            }
            _ => None,
        })
        .unwrap_or_else(|| {
            let calls: Vec<String> = script
                .blocks
                .iter()
                .flat_map(|b| b.instructions.iter())
                .filter_map(|inst| match inst {
                    IRInstruction::Call { callee, .. } => Some(callee.mangled().to_string()),
                    _ => None,
                })
                .collect();
            panic!(
                "no `List.{method_name}` Call instruction in script body — \
                 calls present: {calls:?}",
            )
        });
    script
        .function(&mangled)
        .unwrap_or_else(|| panic!("missing intrinsic `{mangled}` in IRScript"))
}

fn assert_list_intrinsic(function: &IRFunction, method: ListMethod) {
    match &function.kind {
        FunctionKind::Intrinsic(IRIntrinsicId::List(actual)) => assert_eq!(
            *actual, method,
            "intrinsic dispatch landed on the wrong List method",
        ),
        other => panic!(
            "expected `Intrinsic(List({method:?}))` on `{}`, got {other:?}",
            function.symbol,
        ),
    }
    assert!(
        function.blocks.is_empty(),
        "intrinsic body must lower to zero blocks; got {} block(s)",
        function.blocks.len(),
    );
}

#[test]
fn empty_script_lowers_with_autoimport() {
    let script = lower_script_source(&dedent("1"));
    assert_eq!(script.return_type, IRType::Int64);
}

#[test]
fn list_new_lowers_to_intrinsic_new_with_typed_return() {
    let source = "
        my_list: List<Int> = List.new()
        my_list.length()
        ";
    let script = lower_script_source(&dedent(source));
    let new = intrinsic_call(&script, "new");
    assert_list_intrinsic(new, ListMethod::New);
    assert_eq!(new.return_type, IRType::List(Box::new(IRType::Int64)));
}

#[test]
fn list_length_lowers_to_intrinsic_length_returning_int() {
    let source = "
        my_list: List<String> = List.new()
        my_list.length()
        ";
    let script = lower_script_source(&dedent(source));
    let length = intrinsic_call(&script, "length");
    assert_list_intrinsic(length, ListMethod::Length);
    assert_eq!(length.return_type, IRType::Int64);
    assert_eq!(
        length.params[0].ty,
        IRType::List(Box::new(IRType::String)),
        "self param should carry the element-typed List<T>",
    );
}

#[test]
fn list_append_lowers_to_intrinsic_append_with_element_param() {
    let source = "
        my_list: List<Int> = List.new()
        my_list.append(42)
        ";
    let script = lower_script_source(&dedent(source));
    let append = intrinsic_call(&script, "append");
    assert_list_intrinsic(append, ListMethod::Append);
    assert_eq!(append.return_type, IRType::List(Box::new(IRType::Int64)));
    assert_eq!(append.params[1].ty, IRType::Int64);
}

#[test]
fn list_get_lowers_to_intrinsic_get_returning_option() {
    let source = "
        my_list: List<Int> = List.new()
        my_list.get(0)
        ";
    let script = lower_script_source(&dedent(source));
    let get = intrinsic_call(&script, "get");
    assert_list_intrinsic(get, ListMethod::Get);
    let IRType::Enum(symbol) = &get.return_type else {
        panic!(
            "List.get should return `IRType::Enum(Option<T>)`, got {:?}",
            get.return_type,
        );
    };
    assert!(
        symbol.mangled().contains("Option"),
        "List.get return enum should monomorphize to an Option, got `{symbol}`",
    );
}

#[test]
fn list_concat_lowers_to_intrinsic_concat() {
    let source = "
        a: List<Int> = List.new()
        b: List<Int> = List.new()
        a.concat(b)
        ";
    let script = lower_script_source(&dedent(source));
    let concat = intrinsic_call(&script, "concat");
    assert_list_intrinsic(concat, ListMethod::Concat);
    assert_eq!(concat.return_type, IRType::List(Box::new(IRType::Int64)));
}

#[test]
fn list_pop_lowers_to_intrinsic_pop_returning_pair() {
    let source = "
        my_list: List<Int> = List.new()
        my_list.pop()
        ";
    let script = lower_script_source(&dedent(source));
    let pop = intrinsic_call(&script, "pop");
    assert_list_intrinsic(pop, ListMethod::Pop);
    let IRType::Struct(symbol) = &pop.return_type else {
        panic!(
            "List.pop should return `IRType::Struct(Pair<...>)`, got {:?}",
            pop.return_type,
        );
    };
    assert!(
        symbol.mangled().contains("Pair"),
        "List.pop return struct should monomorphize to a Pair, got `{symbol}`",
    );
}

#[test]
fn list_literal_lowers_to_new_plus_append_chain() {
    let source = "
        my_list: List<Int> = [10, 20, 30]
        my_list.length()
        ";
    let script = lower_script_source(&dedent(source));
    let new = intrinsic_call(&script, "new");
    assert_list_intrinsic(new, ListMethod::New);
    let append = intrinsic_call(&script, "append");
    assert_list_intrinsic(append, ListMethod::Append);
    assert_eq!(new.return_type, IRType::List(Box::new(IRType::Int64)));
    assert_eq!(append.return_type, IRType::List(Box::new(IRType::Int64)));
    let append_calls = script
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .filter(|inst| {
            matches!(
                inst,
                IRInstruction::Call { callee, .. }
                    if callee.mangled().contains(".List_") && callee.mangled().ends_with(".append")
            )
        })
        .count();
    assert_eq!(
        append_calls, 3,
        "list literal `[10, 20, 30]` should expand to three `List.append` calls",
    );
}

#[test]
fn list_struct_is_primitive_template_and_skips_struct_decl() {
    let source = "
        my_list: List<Int> = List.new()
        my_list.length()
        ";
    let script = lower_script_source(&dedent(source));
    for package in &script.packages {
        for symbol in package.structs.keys() {
            assert!(
                !symbol.mangled().starts_with("Global.List"),
                "Global.List is a primitive struct template — backends synthesize \
                 storage, no IRStructDecl should be emitted (got `{symbol}`)",
            );
        }
    }
}
