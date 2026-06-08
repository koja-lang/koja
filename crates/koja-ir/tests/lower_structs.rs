//! IR-lowering coverage for the struct slice: `lower/structs.rs`.
//!
//! Walks an end-to-end happy path (`struct Point` plus a body that
//! constructs and projects it) and pins:
//!
//! - `IRPackage::structs` carries the lifted [`IRStructDecl`] keyed
//!   at the mangled [`IRSymbol`], with dense 0..n field indices and
//!   translated [`IRType`]s.
//! - `Point{x: 1, y: 2}` lowers to `IRInstruction::StructInit` with
//!   one [`StructFieldInit`] per declared field, canonicalized to
//!   declaration order regardless of AST order.
//! - `p.x` lowers to `IRInstruction::FieldGet` with the resolved
//!   `field_index`, the field's [`IRType`], and the receiver's
//!   `struct_symbol`.
//! - Feature gaps (generics, struct-fn, annotations, default values)
//!   surface a [`LowerError::Diagnostics`] with the matching message
//!   and the offending struct is dropped from the package fragment.

use koja_ast::util::dedent;
use koja_ir::{IRInstruction, IRScript, IRStructDecl, IRType};

mod common;

use common::{PACKAGE, lower_script_source};

fn struct_decl<'a>(script: &'a IRScript, name: &str) -> &'a IRStructDecl {
    let mangled = format!("{PACKAGE}.{name}");
    script
        .struct_decl(&mangled)
        .unwrap_or_else(|| panic!("struct `{mangled}` missing from IRScript"))
}

// ---------------------------------------------------------------------------
// Decl lowering
// ---------------------------------------------------------------------------

#[test]
fn struct_decl_lowers_to_ir_struct_decl_with_dense_indices() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        ";

    let script = lower_script_source(&dedent(source));
    let decl = struct_decl(&script, "Point");

    assert_eq!(decl.symbol.mangled(), "TestApp.Point");
    assert_eq!(decl.fields.len(), 2);

    assert_eq!(decl.fields[0].name, "x");
    assert_eq!(decl.fields[0].index, 0);
    assert_eq!(decl.fields[0].ir_type, IRType::Int64);

    assert_eq!(decl.fields[1].name, "y");
    assert_eq!(decl.fields[1].index, 1);
    assert_eq!(decl.fields[1].ir_type, IRType::Int64);
}

#[test]
fn mixed_field_struct_translates_each_field_independently() {
    let source = "
        struct Mixed
          flag: Bool
          name: String
          count: Int
        end

        ";

    let script = lower_script_source(&dedent(source));
    let decl = struct_decl(&script, "Mixed");
    let kinds: Vec<_> = decl.fields.iter().map(|f| f.ir_type.clone()).collect();
    assert_eq!(kinds, vec![IRType::Bool, IRType::String, IRType::Int64]);
}

#[test]
fn nested_struct_field_lowers_to_inner_struct_ir_type() {
    let source = "
        struct Inner
          n: Int
        end

        struct Outer
          inner: Inner
          tag: Bool
        end

        ";

    let script = lower_script_source(&dedent(source));
    let outer = struct_decl(&script, "Outer");
    let inner = struct_decl(&script, "Inner");
    assert_eq!(
        outer.fields[0].ir_type,
        IRType::Struct(inner.symbol.clone())
    );
    assert_eq!(outer.fields[1].ir_type, IRType::Bool);
}

#[test]
fn empty_struct_lowers_to_empty_field_list() {
    let source = "
        struct Marker
        end

        ";

    let script = lower_script_source(&dedent(source));
    let decl = struct_decl(&script, "Marker");
    assert!(decl.fields.is_empty());
}

// ---------------------------------------------------------------------------
// StructInit + FieldGet
// ---------------------------------------------------------------------------

#[test]
fn struct_construction_lowers_to_struct_init_with_canonical_field_order() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        Point{y: 20, x: 10}
        ";

    let script = lower_script_source(&dedent(source));
    let block = script.blocks.first().expect("script has one block");
    // Two consts (10, 20) plus the StructInit; AST order doesn't
    // dictate the per-field-init index ordering on the StructInit.
    let init = block
        .instructions
        .iter()
        .find_map(|inst| match inst {
            IRInstruction::StructInit { ty, fields, .. } => Some((ty, fields)),
            _ => None,
        })
        .expect("expected one StructInit");
    let (ty, fields) = init;
    assert_eq!(ty.mangled(), "TestApp.Point");
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].index, 0);
    assert_eq!(fields[1].index, 1);
}

#[test]
fn field_access_lowers_to_field_get_with_resolved_index_and_type() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        Point{x: 1, y: 2}.y
        ";

    let script = lower_script_source(&dedent(source));
    let block = script.blocks.first().expect("script has one block");
    let field_get = block
        .instructions
        .iter()
        .find_map(|inst| match inst {
            IRInstruction::FieldGet {
                field_index,
                field_type,
                struct_symbol,
                ..
            } => Some((*field_index, field_type.clone(), struct_symbol.clone())),
            _ => None,
        })
        .expect("expected one FieldGet");
    let (field_index, field_type, struct_symbol) = field_get;
    assert_eq!(field_index, 1);
    assert_eq!(field_type, IRType::Int64);
    assert_eq!(struct_symbol.mangled(), "TestApp.Point");
    assert_eq!(script.return_type, IRType::Int64);
}

#[test]
fn nested_field_access_chains_two_field_gets() {
    let source = "
        struct Inner
          n: Int
        end

        struct Outer
          inner: Inner
        end

        Outer{inner: Inner{n: 7}}.inner.n
        ";

    let script = lower_script_source(&dedent(source));
    let block = script.blocks.first().expect("script has one block");
    let field_gets: Vec<_> = block
        .instructions
        .iter()
        .filter_map(|inst| match inst {
            IRInstruction::FieldGet {
                field_index,
                struct_symbol,
                ..
            } => Some((*field_index, struct_symbol.mangled().to_string())),
            _ => None,
        })
        .collect();
    assert_eq!(
        field_gets,
        vec![
            (0, "TestApp.Outer".to_string()),
            (0, "TestApp.Inner".to_string()),
        ],
    );
    assert_eq!(script.return_type, IRType::Int64);
}

// Feature-gap diagnostics on the IR side (`lower/structs.rs::has_feature_gap`)
// duplicate the equivalents in `pipeline/collect.rs::diagnose_struct_feature_gaps`.
// In practice the typecheck pass rejects these programs before lowering ever
// runs, so they're unreachable through the normal `parse → check → lower`
// pipeline these tests drive. The IR-side checks stay as defense-in-depth
// for any future caller that bypasses typecheck (e.g. tooling that constructs
// a CheckedProgram by hand); the typecheck-side gaps are covered by
// `koja-typecheck/tests/structs.rs`.

// ---------------------------------------------------------------------------
// Static methods (inline + impl-block forms)
// ---------------------------------------------------------------------------

use koja_ir::FunctionKind;

#[test]
fn inline_static_method_lowers_into_package_function_map() {
    let source = "
        struct Point
          x: Int
          y: Int

          fn origin -> Point
            Point{x: 0, y: 0}
          end
        end

        Point.origin().x
        ";

    let script = lower_script_source(&dedent(source));
    let function = script
        .function("TestApp.Point.origin")
        .expect("inline static method missing from program");
    assert_eq!(function.kind, FunctionKind::Regular);
    assert!(!function.blocks.is_empty(), "method should have a body");
}

#[test]
fn impl_block_static_method_lowers_into_package_function_map() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        extend Point
          fn origin -> Point
            Point{x: 0, y: 0}
          end
        end

        Point.origin().x
        ";

    let script = lower_script_source(&dedent(source));
    let function = script
        .function("TestApp.Point.origin")
        .expect("impl-block static method missing from program");
    assert_eq!(function.kind, FunctionKind::Regular);
    assert!(!function.blocks.is_empty(), "method should have a body");
}

#[test]
fn static_method_call_emits_call_against_qualified_symbol() {
    let source = "
        struct Point
          x: Int
          y: Int

          fn origin -> Point
            Point{x: 0, y: 0}
          end
        end

        Point.origin().x
        ";

    let script = lower_script_source(&dedent(source));
    let block = script.blocks.first().expect("script has one entry block");

    let call_callee = block
        .instructions
        .iter()
        .find_map(|inst| match inst {
            IRInstruction::Call { callee, .. } => Some(callee.clone()),
            _ => None,
        })
        .expect("expected one Call instruction");
    assert_eq!(call_callee.mangled(), "TestApp.Point.origin");

    let field_get = block
        .instructions
        .iter()
        .find_map(|inst| match inst {
            IRInstruction::FieldGet {
                field_index,
                struct_symbol,
                ..
            } => Some((*field_index, struct_symbol.mangled().to_string())),
            _ => None,
        })
        .expect("expected one FieldGet after the Call");
    assert_eq!(field_get, (0, "TestApp.Point".to_string()));
}

#[test]
fn static_method_with_args_lowers_call_with_lowered_args() {
    let source = "
        struct Point
          x: Int

          fn at(seed: Int, _scale: Int) -> Int
            42
          end
        end

        Point.at(7, 3)
        ";

    let script = lower_script_source(&dedent(source));
    let block = script.blocks.first().expect("script has one entry block");
    let (callee, arg_count) = block
        .instructions
        .iter()
        .find_map(|inst| match inst {
            IRInstruction::Call { callee, args, .. } => Some((callee.clone(), args.len())),
            _ => None,
        })
        .expect("expected one Call instruction");
    assert_eq!(callee.mangled(), "TestApp.Point.at");
    assert_eq!(arg_count, 2);
}

// ---------------------------------------------------------------------------
// Instance methods (inline + impl-block forms)
// ---------------------------------------------------------------------------

#[test]
fn inline_instance_method_lowers_with_self_param_promoted() {
    let source = "
        struct Point
          x: Int
          y: Int

          fn first(self) -> Int
            self.x
          end
        end

        Point{x: 1, y: 2}.first()
        ";

    let script = lower_script_source(&dedent(source));
    let method = script
        .function("TestApp.Point.first")
        .expect("inline instance method missing from program");
    assert_eq!(method.kind, FunctionKind::Regular);
    assert_eq!(
        method.params.len(),
        1,
        "instance method should carry exactly one IR param (self)",
    );
    let self_param = &method.params[0];
    assert_eq!(
        self_param.ty,
        IRType::Struct(struct_decl(&script, "Point").symbol.clone()),
        "self's IRType should be the receiver struct",
    );

    let entry = method
        .blocks
        .first()
        .expect("instance method has at least one block");
    assert!(
        entry.instructions.iter().any(
            |i| matches!(i, IRInstruction::LocalDecl { local, .. } if *local == self_param.local_id),
        ),
        "entry block should declare a slot for self: {:?}",
        entry.instructions,
    );
    // `self` is heap-managed (a struct), so promotion *acquires* it:
    // a `Clone` of the incoming param feeds the slot write, giving the
    // frame storage it can drop at exit without disturbing the caller.
    let acquired = entry
        .instructions
        .iter()
        .find_map(|i| match i {
            IRInstruction::Clone { dest, source, .. } if *source == self_param.id => Some(*dest),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "entry block should acquire self before storing it: {:?}",
                entry.instructions,
            )
        });
    assert!(
        entry.instructions.iter().any(|i| matches!(
            i,
            IRInstruction::LocalWrite { local, value, .. }
                if *local == self_param.local_id && *value == acquired,
        )),
        "entry block should store the acquired self into its slot: {:?}",
        entry.instructions,
    );
}

#[test]
fn impl_block_instance_method_lowers_with_self_param_promoted() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        extend Point
          fn first(self) -> Int
            self.x
          end
        end

        Point{x: 1, y: 2}.first()
        ";

    let script = lower_script_source(&dedent(source));
    let method = script
        .function("TestApp.Point.first")
        .expect("impl-block instance method missing from program");
    assert_eq!(method.kind, FunctionKind::Regular);
    assert_eq!(method.params.len(), 1);
    assert_eq!(
        method.params[0].ty,
        IRType::Struct(struct_decl(&script, "Point").symbol.clone()),
    );
}

#[test]
fn instance_method_call_prepends_receiver_to_call_args() {
    let source = "
        struct Point
          x: Int
          y: Int

          fn shift(self, dx: Int) -> Int
            self.x
          end
        end

        Point{x: 1, y: 2}.shift(7)
        ";

    let script = lower_script_source(&dedent(source));
    let block = script.blocks.first().expect("main has one block");

    let (callee, args) = block
        .instructions
        .iter()
        .find_map(|i| match i {
            IRInstruction::Call { callee, args, .. } => Some((callee.clone(), args.clone())),
            _ => None,
        })
        .expect("expected an instance Call in main");
    assert_eq!(callee.mangled(), "TestApp.Point.shift");
    assert_eq!(
        args.len(),
        2,
        "instance call passes (self, dx) — 2 args, receiver-first"
    );

    // The first arg should be the receiver value — the StructInit's
    // dest — the second the lowered explicit `7` Const.
    let init_dest = block
        .instructions
        .iter()
        .find_map(|i| match i {
            IRInstruction::StructInit { dest, .. } => Some(*dest),
            _ => None,
        })
        .expect("expected a StructInit for the receiver");
    assert_eq!(
        args[0], init_dest,
        "first arg of an instance Call must be the receiver value",
    );
}
