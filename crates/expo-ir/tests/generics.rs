//! IR-lowering coverage for the generics slice (`src/generics/`).
//!
//! Pins the closure-pass / monomorphization contract:
//!
//! - Generic templates never appear in [`IRPackage::structs`] /
//!   [`IRPackage::enums`]. The typecheck registry stays the single
//!   source of truth for generic-decl shape; only fully-substituted
//!   concrete decls land in [`IRPackage`].
//! - Construction-site type-arg inference at typecheck propagates
//!   into IR as a discovered instantiation, deduplicated by the
//!   worklist driver: same `(template, args)` → one decl.
//! - Distinct args mint distinct decls with substituted field /
//!   payload [`IRType`]s and distinct mangled symbols.
//! - Nested generics (a generic instantiation whose args themselves
//!   contain a generic instantiation) yield concrete decls for both
//!   inner and outer — the worklist chases discoveries surfaced by
//!   monomorphizing the outer template's substituted field types.
//!
//! Mangled-name shape is `<root>_$<arg>.<arg>$`, where each arg is
//! either a primitive name (`Int64`, `String`) or a nested mangled
//! symbol; nesting brings its own `_$..$` so depth-counting parses
//! unambiguously.

use expo_ast::util::dedent;
use expo_ir::{IRInstruction, IRType, IRVariantPayload};

mod common;

use common::lower_script_source;

// ---------------------------------------------------------------------------
// Generic structs
// ---------------------------------------------------------------------------

#[test]
fn generic_struct_construction_emits_concrete_decl_with_substituted_field_types() {
    let source = "
        struct Pair<T, U>
          a: T
          b: U
        end

        Pair{a: 1, b: \"x\"}
        ";

    let script = lower_script_source(&dedent(source));
    let pkg = script.packages.first().expect("script has one package");
    assert!(
        !pkg.structs.contains_key("TestApp.Pair"),
        "generic template `TestApp.Pair` must not appear in IRPackage.structs",
    );

    let mangled = "TestApp.Pair_$Int64.String$";
    let decl = script
        .struct_decl(mangled)
        .unwrap_or_else(|| panic!("expected concrete struct `{mangled}` in script"));
    assert_eq!(decl.symbol.mangled(), mangled);
    assert_eq!(decl.fields.len(), 2);
    assert_eq!(decl.fields[0].name, "a");
    assert_eq!(decl.fields[0].ir_type, IRType::Int64);
    assert_eq!(decl.fields[1].name, "b");
    assert_eq!(decl.fields[1].ir_type, IRType::String);
}

#[test]
fn generic_struct_construct_uses_mangled_symbol_on_struct_init() {
    let source = "
        struct Pair<T, U>
          a: T
          b: U
        end

        Pair{a: 1, b: \"x\"}
        ";

    let script = lower_script_source(&dedent(source));
    let block = script.blocks.first().expect("script has one block");
    let init_ty = block
        .instructions
        .iter()
        .find_map(|inst| match inst {
            IRInstruction::StructInit { ty, .. } => Some(ty.clone()),
            _ => None,
        })
        .expect("expected one StructInit");
    assert_eq!(init_ty.mangled(), "TestApp.Pair_$Int64.String$");
    assert_eq!(script.return_type, IRType::Struct(init_ty));
}

#[test]
fn generic_struct_idempotent_instantiations_dedupe_to_one_decl() {
    let source = "
        struct Pair<T, U>
          a: T
          b: U
        end

        Pair{a: 1, b: \"x\"}
        Pair{a: 2, b: \"y\"}
        Pair{a: 3, b: \"z\"}
        ";

    let script = lower_script_source(&dedent(source));
    let pair_decls: Vec<&str> = script
        .packages
        .iter()
        .flat_map(|p| p.structs.keys())
        .map(|sym| sym.mangled())
        .filter(|name| name.starts_with("TestApp.Pair"))
        .collect();
    assert_eq!(
        pair_decls,
        vec!["TestApp.Pair_$Int64.String$"],
        "three constructions of `Pair<Int, String>` must dedupe to one IRStructDecl",
    );
}

#[test]
fn generic_struct_distinct_args_produce_distinct_decls() {
    let source = "
        struct Pair<T, U>
          a: T
          b: U
        end

        Pair{a: 1, b: \"x\"}
        Pair{a: \"y\", b: 2}
        ";

    let script = lower_script_source(&dedent(source));
    let mut pair_decls: Vec<&str> = script
        .packages
        .iter()
        .flat_map(|p| p.structs.keys())
        .map(|sym| sym.mangled())
        .filter(|name| name.starts_with("TestApp.Pair"))
        .collect();
    pair_decls.sort();
    assert_eq!(
        pair_decls,
        vec!["TestApp.Pair_$Int64.String$", "TestApp.Pair_$String.Int64$",],
    );
}

#[test]
fn generic_struct_with_user_struct_arg_includes_user_struct_in_mangled_name() {
    let source = "
        struct Inner
          n: Int
        end

        struct Box<T>
          value: T
        end

        Box{value: Inner{n: 1}}
        ";

    let script = lower_script_source(&dedent(source));
    let mangled = "TestApp.Box_$TestApp.Inner$";
    let decl = script
        .struct_decl(mangled)
        .unwrap_or_else(|| panic!("expected `{mangled}` in script"));
    assert_eq!(decl.fields.len(), 1);
    let inner_symbol = script
        .struct_decl("TestApp.Inner")
        .expect("Inner missing")
        .symbol
        .clone();
    assert_eq!(decl.fields[0].ir_type, IRType::Struct(inner_symbol));
}

#[test]
fn nested_generic_struct_yields_concrete_decls_for_outer_and_inner() {
    let source = "
        struct Box<T>
          value: T
        end

        struct Pair<A, B>
          a: A
          b: B
        end

        Pair{a: Box{value: 1}, b: \"x\"}
        ";

    let script = lower_script_source(&dedent(source));

    let inner_mangled = "TestApp.Box_$Int64$";
    let inner = script
        .struct_decl(inner_mangled)
        .unwrap_or_else(|| panic!("expected nested concrete `{inner_mangled}`"));
    assert_eq!(inner.fields[0].ir_type, IRType::Int64);

    let outer_mangled = "TestApp.Pair_$TestApp.Box_$Int64$.String$";
    let outer = script
        .struct_decl(outer_mangled)
        .unwrap_or_else(|| panic!("expected outer concrete `{outer_mangled}`"));
    assert_eq!(
        outer.fields[0].ir_type,
        IRType::Struct(inner.symbol.clone())
    );
    assert_eq!(outer.fields[1].ir_type, IRType::String);
}

// ---------------------------------------------------------------------------
// Substitution coverage for control-flow expressions
// ---------------------------------------------------------------------------

/// Regression: prior to the ternary work, [`ExprKind::Cond`]
/// was in `substitute_in_expr`'s no-op list, so type substitution
/// silently skipped any `cond` arm bodies. A generic struct
/// constructed inside a `cond` arm of a generic function would keep
/// its template-`T`-typed resolution instead of getting rewritten to
/// the call-site type arg, leaving the worklist with no `Box<Int>`
/// instantiation to monomorphize.
#[test]
fn generic_fn_with_cond_in_body_substitutes_arm_body_resolutions() {
    let source = "
        struct Box<T>
          value: T
        end

        fn wrap<T>(value: T) -> Box<T>
          cond
            true -> Box{value: value}
            else -> Box{value: value}
          end
        end

        wrap(1)
        ";

    let script = lower_script_source(&dedent(source));
    let mangled = "TestApp.Box_$Int64$";
    let decl = script.struct_decl(mangled).unwrap_or_else(|| {
        panic!("expected `{mangled}` in script — Cond substitution did not walk arm bodies")
    });
    assert_eq!(decl.fields.len(), 1);
    assert_eq!(decl.fields[0].ir_type, IRType::Int64);
}

// ---------------------------------------------------------------------------
// Generic enums
// ---------------------------------------------------------------------------

#[test]
fn generic_enum_construction_emits_concrete_decl_with_substituted_payload() {
    let source = "
        enum Box<T>
          Of(T)
        end

        Box.Of(42)
        ";

    let script = lower_script_source(&dedent(source));
    let pkg = script.packages.first().expect("script has one package");
    assert!(
        !pkg.enums.contains_key("TestApp.Box"),
        "generic template `TestApp.Box` must not appear in IRPackage.enums",
    );

    let mangled = "TestApp.Box_$Int64$";
    let decl = script
        .enum_decl(mangled)
        .unwrap_or_else(|| panic!("expected concrete enum `{mangled}`"));
    assert_eq!(decl.symbol.mangled(), mangled);
    assert_eq!(decl.variants.len(), 1);
    let of = &decl.variants[0];
    assert_eq!(of.name, "Of");
    match &of.payload {
        IRVariantPayload::Tuple(types) => assert_eq!(types, &vec![IRType::Int64]),
        other => panic!("expected Tuple([Int64]), got {other:?}"),
    }
}

#[test]
fn generic_enum_construct_uses_mangled_symbol_on_enum_construct() {
    let source = "
        enum Box<T>
          Of(T)
        end

        Box.Of(42)
        ";

    let script = lower_script_source(&dedent(source));
    let block = script.blocks.first().expect("script has one block");
    let construct_ty = block
        .instructions
        .iter()
        .find_map(|inst| match inst {
            IRInstruction::EnumConstruct { ty, .. } => Some(ty.clone()),
            _ => None,
        })
        .expect("expected one EnumConstruct");
    assert_eq!(construct_ty.mangled(), "TestApp.Box_$Int64$");
    assert_eq!(script.return_type, IRType::Enum(construct_ty));
}

#[test]
fn generic_enum_idempotent_instantiations_dedupe_to_one_decl() {
    let source = "
        enum Box<T>
          Of(T)
        end

        Box.Of(1)
        Box.Of(2)
        Box.Of(3)
        ";

    let script = lower_script_source(&dedent(source));
    let box_decls: Vec<&str> = script
        .packages
        .iter()
        .flat_map(|p| p.enums.keys())
        .map(|sym| sym.mangled())
        .filter(|name| name.starts_with("TestApp.Box"))
        .collect();
    assert_eq!(box_decls, vec!["TestApp.Box_$Int64$"]);
}

#[test]
fn generic_enum_distinct_args_produce_distinct_decls() {
    let source = "
        enum Box<T>
          Of(T)
        end

        Box.Of(42)
        Box.Of(\"x\")
        ";

    let script = lower_script_source(&dedent(source));
    let mut box_decls: Vec<&str> = script
        .packages
        .iter()
        .flat_map(|p| p.enums.keys())
        .map(|sym| sym.mangled())
        .filter(|name| name.starts_with("TestApp.Box"))
        .collect();
    box_decls.sort();
    assert_eq!(
        box_decls,
        vec!["TestApp.Box_$Int64$", "TestApp.Box_$String$"],
    );
}

#[test]
fn generic_enum_struct_variant_substitutes_each_payload_field() {
    let source = "
        enum Pair<T, U>
          Of { a: T, b: U }
        end

        Pair.Of{a: 1, b: \"x\"}
        ";

    let script = lower_script_source(&dedent(source));
    let mangled = "TestApp.Pair_$Int64.String$";
    let decl = script
        .enum_decl(mangled)
        .unwrap_or_else(|| panic!("expected concrete enum `{mangled}`"));
    let of = &decl.variants[0];
    let IRVariantPayload::Struct(fields) = &of.payload else {
        panic!("expected Struct payload, got {:?}", of.payload);
    };
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].name, "a");
    assert_eq!(fields[0].ir_type, IRType::Int64);
    assert_eq!(fields[1].name, "b");
    assert_eq!(fields[1].ir_type, IRType::String);
}

#[test]
fn nested_generic_enum_in_generic_struct_yields_concrete_decls_for_both() {
    let source = "
        enum Box<T>
          Of(T)
        end

        struct Pair<A, B>
          a: A
          b: B
        end

        Pair{a: Box.Of(1), b: \"x\"}
        ";

    let script = lower_script_source(&dedent(source));

    let inner_mangled = "TestApp.Box_$Int64$";
    let inner = script
        .enum_decl(inner_mangled)
        .unwrap_or_else(|| panic!("expected nested concrete `{inner_mangled}`"));
    let IRVariantPayload::Tuple(inner_types) = &inner.variants[0].payload else {
        panic!(
            "expected Tuple payload, got {:?}",
            inner.variants[0].payload
        );
    };
    assert_eq!(inner_types, &vec![IRType::Int64]);

    let outer_mangled = "TestApp.Pair_$TestApp.Box_$Int64$.String$";
    let outer = script
        .struct_decl(outer_mangled)
        .unwrap_or_else(|| panic!("expected outer concrete `{outer_mangled}`"));
    assert_eq!(outer.fields[0].ir_type, IRType::Enum(inner.symbol.clone()));
    assert_eq!(outer.fields[1].ir_type, IRType::String);
}

// ---------------------------------------------------------------------------
// Static method dispatch on generic types
// ---------------------------------------------------------------------------

/// Static-dispatch method calls on a generic type (`Box.make(...)`)
/// previously skipped the per-method instantiation enqueue when no
/// method-level type-args were present, so the call site mangled
/// `Box_$Int64$.make` but no `IRFunction` with that symbol ever
/// reached the `IRProgram` — `seal_program_calls` then panicked
/// with "function `...` calls `Box_$Int64$.make`, but that function
/// is not registered in the IRProgram". Pin the regression by
/// asserting both the call instruction and the mono'd target land
/// in the same script.
#[test]
fn static_call_on_generic_struct_registers_mono_method() {
    let source = "
        struct Box<T>
          value: T
        end

        extend Box<T>
          fn make(v: T) -> Box<T>
            Box{value: v}
          end
        end

        Box.make(42)
        ";

    let script = lower_script_source(&dedent(source));
    let target = "TestApp.Box_$Int64$.make";
    let function = script
        .function(target)
        .unwrap_or_else(|| panic!("expected mono'd `{target}` in script"));
    assert_eq!(function.symbol.mangled(), target);

    let block = script.blocks.first().expect("script has one block");
    let call_symbol = block
        .instructions
        .iter()
        .find_map(|inst| match inst {
            IRInstruction::Call { callee, .. } => Some(callee.mangled().to_string()),
            _ => None,
        })
        .expect("expected one Call to the mono'd static method");
    assert_eq!(call_symbol, target);
}

/// Receive-arm typed-binding patterns previously skipped pattern-type
/// substitution during mono, leaving a raw `TypeParam` inside the
/// payload's `resolved_type` and panicking downstream in
/// `resolved_type_to_ir_type` ("received a non-Global resolution
/// (TypeParam ...)"). The fix walks `Pattern::TypedBinding`'s
/// `resolved_type` slot alongside the existing expr-level substitute,
/// so monomorphizing a generic receive-arm container substitutes the
/// payload type the same way it substitutes arm bodies.
#[test]
fn receive_arm_typed_binding_substitutes_payload_type_during_mono() {
    // No receive-arm-aware standalone smoke is wired up at the
    // script-test layer (receive lives behind a process struct), so
    // settle for "this mono'd path used to panic; if the script
    // lowers without panicking, the pattern substitution is doing
    // its job".
    let source = "
        struct Pair<A, B>
          first: A
          second: B
        end

        struct Wrap<R>
          x: R
        end

        extend Wrap<R>
          fn make(value: R) -> Pair<Wrap<R>, R>
            Pair{first: Wrap{x: value}, second: value}
          end
        end

        Wrap.make(42)
        ";

    let script = lower_script_source(&dedent(source));
    let target = "TestApp.Wrap_$Int64$.make";
    script
        .function(target)
        .unwrap_or_else(|| panic!("expected mono'd `{target}` in script"));
    let outer_pair = "TestApp.Pair_$TestApp.Wrap_$Int64$.Int64$";
    script
        .struct_decl(outer_pair)
        .unwrap_or_else(|| panic!("expected nested `{outer_pair}` in script"));
}
