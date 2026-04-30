//! Lowering for loop constructs (`while`, `loop`, `for`) plus the
//! `Enumeration` impl-dispatch resolver `for` loops use at emission.
//!
//! ## Construct lifts (Slice 6)
//!
//! [`Lowerer::lower_while`] / [`Lowerer::lower_loop`] /
//! [`Lowerer::lower_for`] mirror
//! [`Lowerer::lower_if_no_else`](crate::Lowerer::lower_if_no_else) /
//! [`Lowerer::lower_unless`](crate::Lowerer::lower_unless): mint
//! fresh per-construct [`IRBlockId`](crate::blocks::IRBlockId)s,
//! lower the cond expression (where present) into the header's
//! instruction sequence via
//! [`Lowerer::lower_expr_to_operand`](crate::Lowerer::lower_expr_to_operand),
//! and record the canonicalized branch on the resulting
//! `IR*` value. Bodies remain AST `Vec<Statement>` stubs walked by
//! emission until Phase 4g (statement-level lowering).
//!
//! `for` keeps the iterable + binding pattern as AST stubs because
//! the iterator-protocol desugaring (`length()` + `get()` + `Option`
//! unwrap + pattern bind) lives at the codegen seam where the LLVM
//! type registry is reachable; the lowerer only mints the block ids
//! and the value-map slots the emit walker stuffs the iterable /
//! index allocas into.
//!
//! ## `Enumeration` dispatch (`for` loops)
//!
//! `for item in iterable` desugars at emission time to an indexed `while`
//! loop calling `iterable.length()` and `iterable.get(idx)`. To pick the
//! right impl methods and bind `item` with the right LLVM type, the
//! emitter needs the iterable's mangled type key, base name, type-args,
//! and element Expo type. [`resolve_enumerable_info`] computes all of
//! that against the type registry; emission then derives the LLVM
//! element type with one `to_llvm_type(...)` call.

use expo_ast::ast::{Expr, Pattern, Statement};
use expo_typecheck::types::{Type, build_substitution, mangle_name, substitute_preserving};

use crate::Lowerer;
use crate::blocks::IRTerminator;
use crate::identity::MonomorphizedTypeIdentifier;
use crate::lower::ctx::LowerCtx;
use crate::lower::mangling::try_parse_mangled_name;
use crate::lower::types::resolve_name_current;
use crate::resolved::loops::{IRFor, IRLoop, IRWhile, ResolvedEnumerable};

impl<'a> Lowerer<'a> {
    /// Lowers a `loop ... end` (infinite loop). Mints `body_block` /
    /// `exit_block`; body terminator unconditionally branches back to
    /// `body_block`. The exit block exists so AST `break` statements
    /// can branch to it via the surrounding emit walker's
    /// `loop_exit_stack` (until Phase 4g lifts `break` into IR).
    pub fn lower_loop(&mut self, body: &[Statement]) -> Result<IRLoop, String> {
        let body_id = self.next_block_id();
        let exit_block = self.next_block_id();
        let body = self.lower_body_block(body_id, "loop_body", body, body_id)?;
        Ok(IRLoop { body, exit_block })
    }

    /// Lowers a `while cond ... end`. Mints `header_block` /
    /// `body_block` / `exit_block`; lowers `cond` into
    /// `header_instructions` via
    /// [`Self::lower_expr_to_operand`](crate::Lowerer::lower_expr_to_operand).
    /// Header terminator is the canonicalized `CondBranch { cond,
    /// then: body_block, otherwise: exit_block }`; body terminator
    /// branches back to the header (re-evaluating the cond each
    /// iteration).
    pub fn lower_while(&mut self, cond: &Expr, body: &[Statement]) -> Result<IRWhile, String> {
        let header_block = self.next_block_id();
        let body_id = self.next_block_id();
        let exit_block = self.next_block_id();
        let mut header_instructions = Vec::new();
        let cond_operand = self.lower_expr_to_operand(&mut header_instructions, cond);
        let body = self.lower_body_block(body_id, "while_body", body, header_block)?;
        Ok(IRWhile {
            body,
            exit_block,
            header_block,
            header_instructions,
            header_terminator: IRTerminator::CondBranch {
                cond: cond_operand,
                then: body_id,
                otherwise: exit_block,
            },
        })
    }

    /// Lowers a `for binding in iterable ... end`. Mints
    /// `header_block` / `body_block` / `exit_block` and pre-allocates
    /// `iterable_value` / `idx_value` slots in the function-scoped
    /// value map (the emit walker stuffs the iterable's stack-stored
    /// alloca pointer and the index alloca pointer into them so
    /// future IR instructions can reference them via
    /// [`IROperand::Local`](crate::values::IROperand::Local)).
    ///
    /// The iterable expression and the binding pattern stay AST-stubbed
    /// because the iterator-protocol desugaring (calls `length()` and
    /// `get()`, unwraps the `Option`, then binds via the pattern)
    /// lives at the codegen seam where the LLVM type registry is
    /// reachable. Same precedent as
    /// [`crate::values::IRInstruction::PatternBinaryMatch`] from Slice 5b.
    pub fn lower_for(
        &mut self,
        iterable: &Expr,
        binding_pattern: &Pattern,
        body: &[Statement],
    ) -> Result<IRFor, String> {
        let body_id = self.next_block_id();
        let exit_block = self.next_block_id();
        let header_block = self.next_block_id();
        let idx_value = self.next_value_id();
        let iterable_value = self.next_value_id();
        let body = self.lower_body_block(body_id, "for_body", body, header_block)?;
        Ok(IRFor {
            binding_pattern: binding_pattern.clone(),
            body,
            exit_block,
            header_block,
            idx_value,
            iterable: iterable.clone(),
            iterable_value,
        })
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
