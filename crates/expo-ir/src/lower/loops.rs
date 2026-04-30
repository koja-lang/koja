//! Lowering for loop constructs (`while`, `loop`, `for`).
//!
//! Each lowering takes a `&mut CFGBuilder` plus the currently-open
//! [`IRBlockId`], mints fresh per-construct block ids, wires the
//! loop scaffolding (header / body / exit), pushes the exit id onto
//! [`crate::FnLowerState::loop_exit`] for inner `break` resolution,
//! and returns the new open block (the exit) plus
//! [`IROperand::Unit`] (loops are statement-shaped).
//!
//! ## `for` loops and `IRInstruction::ForLoopStub`
//!
//! `for` keeps the iterable expression and binding pattern alongside
//! a structural placeholder ([`IRInstruction::ForLoopStub`]; planned
//! in Slice 3c). The pre-codegen elaboration pass expands the stub
//! into the iterator-protocol multi-block desugar (`length()` /
//! `get()` / `Option` unwrap / pattern bind / `idx++`), calling
//! [`crate::lower::monomorphize::monomorphize_impl_method`] on
//! `length` / `get` to register the impl methods in `IRProgram`.
//! Until 3c lands, `lower_for` Stubs the entire `for` expression and
//! the codegen shim handles emission directly.
//!
//! ## `Enumeration` dispatch resolution
//!
//! [`resolve_enumerable_info`] consumes the iterable's `Type` and
//! produces a [`ResolvedEnumerable`] -- the mangled type, base name,
//! type-args, and element type the elaboration pass needs.

use expo_ast::ast::{Expr, Statement};
use expo_typecheck::types::{Type, build_substitution, mangle_name, substitute_preserving};

use crate::Lowerer;
use crate::blocks::{IRBlockId, IRTerminator};
use crate::cfg::CFGBuilder;
use crate::identity::MonomorphizedTypeIdentifier;
use crate::lower::ctx::LowerCtx;
use crate::lower::mangling::try_parse_mangled_name;
use crate::lower::types::resolve_name_current;
use crate::resolved::loops::ResolvedEnumerable;
use crate::values::{IRInstruction, IROperand};

impl<'a> Lowerer<'a> {
    /// Lower an infinite `loop ... end`. Two blocks: a body block
    /// whose declared terminator is `Branch(body)` (the back-edge --
    /// overridden by `break` / `return` / `panic`), and an exit
    /// block that subsequent control flow lands in.
    pub fn lower_loop(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        body: &[Statement],
    ) -> Result<(Option<IRBlockId>, IROperand), String> {
        let body_id = self.next_block_id();
        let exit_id = self.next_block_id();

        builder.set_terminator(open, IRTerminator::Branch(body_id));

        // Push the loop's exit id onto `FnLowerState::loop_exit` so
        // any [`Statement::Break`] reachable through this body --
        // including ones lowered later at execute time inside a
        // Stub-deferred control-flow expression -- resolves to the
        // right exit. The codegen-side `compile_loop` shim pops
        // after the walk completes.
        self.fn_state.push_loop_exit(exit_id);
        self.lower_body_block(builder, body_id, "loop_body", body, body_id)?;

        builder.add_block(exit_id, "loop_exit");
        Ok((Some(exit_id), IROperand::Unit))
    }

    /// Lower a `while cond ... end`. Three blocks: a header (cond
    /// lift + canonicalized `CondBranch { cond, then: body, otherwise:
    /// exit }`), a body whose back-edge terminator branches to
    /// header, and an exit block.
    pub fn lower_while(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        cond: &Expr,
        body: &[Statement],
    ) -> Result<(Option<IRBlockId>, IROperand), String> {
        let header_id = self.next_block_id();
        let body_id = self.next_block_id();
        let exit_id = self.next_block_id();

        builder.set_terminator(open, IRTerminator::Branch(header_id));

        builder.add_block(header_id, "while_header");
        let (header_exit, cond_op, _) = self.lower_expr_to_operand(builder, header_id, cond)?;
        let Some(header_exit) = header_exit else {
            // Cond lowering terminated all paths; impossible in
            // well-typed code but propagate defensively.
            return Ok((None, IROperand::Unit));
        };
        builder.set_terminator(
            header_exit,
            IRTerminator::CondBranch {
                cond: cond_op,
                then: body_id,
                otherwise: exit_id,
            },
        );

        // Codegen-side `compile_while` pops loop_exit after the
        // walk completes; pushing here lets break statements
        // inside Stub-deferred sub-expressions resolve to `exit_id`
        // at execute time too.
        self.fn_state.push_loop_exit(exit_id);
        self.lower_body_block(builder, body_id, "while_body", body, header_id)?;

        builder.add_block(exit_id, "while_exit");
        Ok((Some(exit_id), IROperand::Unit))
    }

    /// Lower a `for binding in iterable ... end`. Today: emits a
    /// fallback [`IRInstruction::Stub`] wrapping the full `for`
    /// expression (codegen routes through `compile_for`). Once the
    /// elaboration pass lands in Slice 3c, switch to emitting an
    /// [`IRInstruction::ForLoopStub`] that elaboration expands into
    /// the iterator-protocol desugar.
    pub fn lower_for(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        for_expr: &Expr,
    ) -> Result<(Option<IRBlockId>, IROperand), String> {
        let dest = self.next_value_id();
        builder.append(
            open,
            IRInstruction::Stub {
                dest,
                expr: Box::new(for_expr.clone()),
                result_type: Type::Unit,
            },
        );
        // The Stub's compile_expr path returns Unit (a `for` loop has
        // no value). Continue lowering on the same open block.
        Ok((Some(open), IROperand::Unit))
    }
}

/// Resolves the `Enumeration` impl to dispatch through for `for item in
/// iterable`. Validates that `ty`'s base implements `Enumeration`,
/// computes the mangled type key (= symbol prefix for `length` / `get`),
/// and derives the element Expo type from the `get` method's
/// `Option<T>` return signature.
pub fn resolve_enumerable_info(
    ctx: &LowerCtx<'_>,
    ty: &Type,
) -> Result<ResolvedEnumerable, String> {
    let (base, type_args) = base_and_type_args(ctx, ty)?;

    let base_id = resolve_name_current(ctx, &base)
        .ok_or_else(|| format!("no type info for `{base}`"))?
        .clone();

    let protos = ctx
        .type_ctx
        .protocol_impls
        .get(&base_id)
        .ok_or_else(|| format!("`{base}` does not implement the Enumeration protocol"))?;
    if !protos.iter().any(|(p, _)| p == "Enumeration") {
        return Err(format!(
            "`{base}` does not implement the Enumeration protocol"
        ));
    }

    let ti = ctx
        .type_ctx
        .get_type(&base_id)
        .ok_or_else(|| format!("no type info for `{base}`"))?;
    let get_sig = ti
        .functions
        .get("get")
        .ok_or_else(|| format!("`{base}` implements Enumeration but has no `get` method"))?;

    let option_ty = if ti.type_params.is_empty() {
        get_sig.return_type.clone()
    } else {
        let subst = build_substitution(&ti.type_params, &type_args);
        substitute_preserving(&get_sig.return_type, &subst)
    };
    let elem_type = match &option_ty {
        Type::Named {
            identifier,
            type_args: ta,
        } if identifier.name == "Option" && !ta.is_empty() => ta[0].clone(),
        other => other.clone(),
    };

    let mangled_type = MonomorphizedTypeIdentifier::new(mangle_name(&base_id, &type_args));

    Ok(ResolvedEnumerable {
        base,
        elem_type,
        mangled_type,
        type_args,
    })
}

/// Splits a candidate iterable type into its base name and type-args.
/// Mangled monomorphized names (`List_$Int32$`) are unparsed back into
/// their components; primitives carry their Expo display name as the
/// base (so `Enumeration` impls on `String`, etc., resolve uniformly).
fn base_and_type_args(ctx: &LowerCtx<'_>, ty: &Type) -> Result<(String, Vec<Type>), String> {
    match ty {
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => Ok((identifier.name.clone(), type_args.clone())),
        Type::Named { identifier, .. } => {
            try_parse_mangled_name(ctx, &identifier.name).ok_or_else(|| not_enumerable_error(ty))
        }
        Type::Primitive(primitive) => Ok((primitive.display().to_string(), Vec::new())),
        _ => Err(not_enumerable_error(ty)),
    }
}

fn not_enumerable_error(ty: &Type) -> String {
    format!(
        "`for` requires an Enumeration type, found `{}`",
        ty.display()
    )
}
