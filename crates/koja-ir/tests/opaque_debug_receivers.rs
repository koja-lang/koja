//! Regression coverage for the opaque-receiver shortcut in
//! [`koja_ir::lower::calls::lower_method_call`]. When a bounded
//! `Debug.{format, print, inspect}` call's receiver, post
//! monomorphic substitution, resolves to an anonymous type
//! ([`ResolvedType::Union`] or
//! [`ResolvedType::Anonymous(AnonymousKind::Function)`]), the call
//! emits a constant `"..."` placeholder instead of routing through
//! `receiver_struct_id`.
//!
//! This guards the bug originally hit through the stdlib's (since
//! removed) parametric `impl Debug for Pair<A, B>`, whose body calls
//! `self.first.format()` on a type-parameter receiver. Substituting
//! `A -> Union<...>` at monomorphization time produces a method call
//! whose receiver resolution is `Union(...)`, which the old
//! `receiver_struct_id` would reject as a non-`Named { Global }`
//! value, hence the panic we hit when loading `Net.tcp.koja`'s
//! `Process<TCPServerConfig, TCPServerMsg | IOReady, String>` impl.
//! The fixtures below declare their own equivalent parametric impl.
//!
//! The behavioral contract mirrors `derive_debug`'s opaque-field
//! rule at the AST layer
//! ([`koja_typecheck::pipeline::synthesize::derive_debug::is_opaque_type`]),
//! where anonymous types render as the literal `"..."`. Keep the
//! two layers in sync.

use koja_ir::{ConstValue, IRFunction, IRInstruction};

mod common;

use common::{all_instructions, lower_script_source, script_function_names};

/// Generic two-field struct with the parametric `Debug` impl shape
/// that used to live on the stdlib `Pair`: the body calls `.format()`
/// on type-parameter receivers.
const DUO_SNIPPET: &str = "
        struct Duo<A, B>
          first: A
          second: B
        end

        impl Debug for Duo<A, B>
          fn format(self) -> String
            \"(\" <> self.first.format() <> \", \" <> self.second.format() <> \")\"
          end
        end
        ";

/// Walk every `IRInstruction::Const` in `function`'s blocks and
/// collect the string literals folded into them. Used to assert
/// that the opaque-receiver shortcut materialized the `"..."`
/// placeholder for the union side of a `Duo<Union, _>.format` mono.
fn collect_string_consts(function: &IRFunction) -> Vec<String> {
    all_instructions(&function.blocks)
        .filter_map(|instr| match instr {
            IRInstruction::Const {
                value: ConstValue::String(s),
                ..
            } => Some(s.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn pair_of_union_format_substitutes_to_opaque_placeholder() {
    // `Duo<A, B>.format()`'s parametric body calls
    // `self.first.format()`. With `A = Foo | Bar`, the substituted
    // body's receiver is `Union(...)`. Without the opaque shortcut
    // this would panic in `receiver_struct_id`. We exercise the path
    // by passing a union-typed pair into `format`.
    let source = format!(
        "{DUO_SNIPPET}
        enum Foo
          F
        end

        enum Bar
          B
        end

        fn widen(value: Foo | Bar) -> Foo | Bar
          value
        end

        pair: Duo<Foo | Bar, Int> = Duo{{first: widen(Foo.F), second: 1}}
        _ = pair.format()
        0
        "
    );

    let script = lower_script_source(&source);
    let mono = script
        .packages
        .iter()
        .flat_map(|p| p.functions.iter())
        .find(|(sym, _)| {
            let m = sym.mangled();
            m.starts_with("TestApp.Duo_$Union_") && m.ends_with(".format")
        })
        .map(|(_, function)| function)
        .expect("expected a `TestApp.Duo_$Union_...$.format` mono in IRProgram");
    let consts = collect_string_consts(mono);
    assert!(
        consts.iter().any(|s| s == "..."),
        "expected opaque `...` placeholder in Duo format mono; saw consts {consts:?}",
    );
}

#[test]
fn pair_with_function_field_format_substitutes_to_opaque_placeholder() {
    // The same shortcut covers
    // `ResolvedType::Anonymous(AnonymousKind::Function)`. Here `A`
    // lands as a function type rather than a union, but the bounded
    // `.format()` call shape is identical. Latent before this slice.
    // Pinned here so a future refactor doesn't re-introduce the
    // panic for closure-typed type-param values.
    let source = format!(
        "{DUO_SNIPPET}
        inc = fn (x: Int) -> Int x + 1 end
        pair: Duo<fn (Int) -> Int, Int> = Duo{{first: inc, second: 1}}
        _ = pair.format()
        0
        "
    );

    let script = lower_script_source(&source);
    let all_duo_format_mangles: Vec<String> = script_function_names(&script)
        .into_iter()
        .filter(|m| m.starts_with("TestApp.Duo_$") && m.ends_with(".format"))
        .collect();
    // The exact mangle for a function-typed arg includes a `Fn`
    // marker that ir can rename later. Pin the search to that
    // marker rather than the full shape.
    let mono = script
        .packages
        .iter()
        .flat_map(|p| p.functions.iter())
        .find(|(sym, _)| {
            let m = sym.mangled();
            m.starts_with("TestApp.Duo_$Fn") && m.ends_with(".format")
        })
        .map(|(_, function)| function)
        .unwrap_or_else(|| {
            panic!(
                "expected a `TestApp.Duo_$Fn...$.format` mono in IRProgram; saw \
                 {all_duo_format_mangles:#?}",
            )
        });
    let consts = collect_string_consts(mono);
    assert!(
        consts.iter().any(|s| s == "..."),
        "expected opaque `...` placeholder for function-typed Duo element; saw {consts:?}",
    );
}
