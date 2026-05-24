//! Regression coverage for the opaque-receiver shortcut in
//! [`koja_ir::lower::calls::lower_method_call`]: when a bounded
//! `Debug.{format, print, inspect}` call's receiver, post
//! monomorphic substitution, resolves to an anonymous type
//! ([`ResolvedType::Union`] or
//! [`ResolvedType::Anonymous(AnonymousKind::Function)`]), the call
//! emits a constant `"..."` placeholder instead of routing through
//! `receiver_struct_id`.
//!
//! The bug this guards: the stdlib hands out a parametric
//! `impl Debug for Pair<A, B>` whose body calls `self.first.format()`
//! on a type-parameter receiver. Substituting `A → Union<...>` at
//! monomorphization time produces a method call whose receiver
//! resolution is `Union(...)`, which the old `receiver_struct_id`
//! would reject as a non-`Named { Global }` value — hence the panic
//! we hit when loading `Net.tcp.koja`'s
//! `Process<TCPServerConfig, TCPServerMsg | IOReady, String>` impl
//! (it synthesizes `TCPServer.run` whose receive arm binds
//! `pair: Pair<Union<TCPServerMsg, IOReady>, Option<...>>`, and
//! `enqueue_member_methods` then monomorphizes `Pair.format` for
//! that exact shape).
//!
//! The behavioral contract mirrors `derive_debug`'s opaque-field
//! rule at the AST layer
//! ([`koja_typecheck::pipeline::synthesize::derive_debug::is_opaque_type`]):
//! anonymous types render as the literal `"..."`. Keep the two
//! layers in sync.

use koja_ast::util::dedent;
use koja_ir::IRFunction;

mod common;

use common::lower_program_source;

/// Walk every `IRInstruction::Const` in `function`'s blocks and
/// collect the string literals folded into them. Used to assert
/// that the opaque-receiver shortcut materialized the `"..."`
/// placeholder for the union side of a `Pair<Union, _>.format` mono.
fn collect_string_consts(function: &IRFunction) -> Vec<String> {
    use koja_ir::{ConstValue, IRInstruction};
    let mut out: Vec<String> = Vec::new();
    for block in &function.blocks {
        for instr in &block.instructions {
            if let IRInstruction::Const {
                value: ConstValue::String(s),
                ..
            } = instr
            {
                out.push(s.clone());
            }
        }
    }
    out
}

#[test]
fn pair_of_union_format_substitutes_to_opaque_placeholder() {
    // `Pair<A, B>.format()`'s parametric body calls
    // `self.first.format()`. With `A = Foo | Bar`, the substituted
    // body's receiver is `Union(...)` — without the opaque shortcut
    // this would panic in `receiver_struct_id`. We exercise the path
    // by passing a union-typed pair into `format`.
    let source = "
        enum Foo
          F
        end

        enum Bar
          B
        end

        fn widen(value: Foo | Bar) -> Foo | Bar
          value
        end

        fn main
          pair: Pair<Foo | Bar, Int> = Pair{first: widen(Foo.F), second: 1}
          _ = pair.format()
          0
        end
        ";

    let program = lower_program_source(&dedent(source));
    let mono = program
        .packages
        .iter()
        .flat_map(|p| p.functions.iter())
        .find(|(sym, _)| {
            let m = sym.mangled();
            m.starts_with("Global.Pair_$Union_") && m.ends_with(".format")
        })
        .map(|(_, function)| function)
        .expect("expected a `Global.Pair_$Union_...$.format` mono in IRProgram");
    let consts = collect_string_consts(mono);
    assert!(
        consts.iter().any(|s| s == "..."),
        "expected opaque `...` placeholder in Pair format mono; saw consts {consts:?}",
    );
}

#[test]
fn pair_with_function_field_format_substitutes_to_opaque_placeholder() {
    // The same shortcut covers
    // `ResolvedType::Anonymous(AnonymousKind::Function)` — `A` lands
    // as a function type rather than a union, but the bounded
    // `.format()` call shape is identical. Latent before this slice;
    // pinned here so a future refactor doesn't re-introduce the
    // panic for closure-typed type-param values.
    let source = "
        fn main
          inc = fn (x: Int) -> Int x + 1 end
          pair: Pair<fn (Int) -> Int, Int> = Pair{first: inc, second: 1}
          _ = pair.format()
          0
        end
        ";

    let program = lower_program_source(&dedent(source));
    let all_pair_format_mangles: Vec<String> = program
        .packages
        .iter()
        .flat_map(|p| p.functions.keys())
        .map(|sym| sym.mangled().to_string())
        .filter(|m| m.starts_with("Global.Pair_$") && m.ends_with(".format"))
        .collect();
    // The exact mangle for a function-typed arg includes a `Fn`
    // marker that ir can rename later; pin the search to the
    // mono that wasn't there before (anything except the bare
    // `Pair_$.Int64$.format` autoimport shape).
    let mono = program
        .packages
        .iter()
        .flat_map(|p| p.functions.iter())
        .find(|(sym, _)| {
            let m = sym.mangled();
            m.starts_with("Global.Pair_$Fn") && m.ends_with(".format")
        })
        .map(|(_, function)| function)
        .unwrap_or_else(|| {
            panic!(
                "expected a `Global.Pair_$Fn...$.format` mono in IRProgram; saw \
                 {all_pair_format_mangles:#?}",
            )
        });
    let consts = collect_string_consts(mono);
    assert!(
        consts.iter().any(|s| s == "..."),
        "expected opaque `...` placeholder for function-typed Pair element; saw {consts:?}",
    );
}
