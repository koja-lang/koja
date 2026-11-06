//! Hashtable key conformances the backend calls by symbol.
//!
//! The LLVM `Map` and `Set` intrinsics hash and compare keys through
//! `<K>.hash/1` and `<K>.equals?/2`, looked up by name at codegen.
//! Nothing else guarantees those symbols exist. A protocol method on
//! a generic type instantiates only from a call site, and a union has
//! no decl to hang an impl on. [`enqueue_key_conformances`] runs once
//! per `Map` or `Set` instantiation and closes both gaps, so every key
//! that passed typecheck's `Hash & Equality` bound has both symbols
//! before seal.

use koja_ast::identifier::{LocalId, ResolvedType};
use koja_typecheck::{GlobalRegistry, peel_alias};

use super::calls::conformance_method_symbol;
use super::closures::read_wrapper_args;
use super::ctx::{FnLowerCtx, LowerOutput};
use super::drops::emit_function_exit_drops;
use super::equality::lower_value_equality;
use super::ownership::promote_param;
use super::package::resolved_type_to_ir_type;
use super::unions::{UnionSubject, emit_union_hash};
use crate::function::{FunctionKind, IRBlockId, IRFunction, IRSymbol, IRTerminator};
use crate::local::IRLocalId;
use crate::mangling::mangled_method_name;
use crate::types::{IRType, ValueId};

/// Make sure `key`'s `hash/1` and `equals?/2` exist as callable
/// symbols. A nominal key goes through [`conformance_method_symbol`],
/// which enqueues the instantiation for a generic receiver and is a
/// plain lookup for a concrete one. A union key gets a synthesized
/// pair, see [`synthesize_union_hash_eq`].
pub(crate) fn enqueue_key_conformances(
    key: &ResolvedType,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) {
    let structural = peel_alias(key, registry);
    match &structural {
        ResolvedType::Named { .. } => {
            conformance_method_symbol(&structural, "hash", 1, registry, output);
            conformance_method_symbol(&structural, "equals?", 2, registry, output);
        }
        ResolvedType::Union(members) => synthesize_union_hash_eq(members, registry, output),
        other => panic!(
            "IR lower: hashtable key `{other:?}` is neither nominal nor a union \
             (typecheck's `Hash & Equality` bound admits nothing else)",
        ),
    }
}

/// Build `Union_X.hash/1` and `Union_X.equals?/2` for a union key,
/// once per distinct union. The bodies are the inline expansions a
/// `u.hash()` or `u == v` site lowers to, wrapped in a function
/// shaped like a user `fn hash(self)`, so the hashtable intrinsics
/// call them the way they call a struct's impl.
fn synthesize_union_hash_eq(
    members: &[ResolvedType],
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) {
    let union_resolved = ResolvedType::Union(members.to_vec());
    let union_ty = resolved_type_to_ir_type(&union_resolved, registry, &mut output.instantiations);
    let IRType::Union { mangled, .. } = &union_ty else {
        panic!("IR lower: union key `{members:?}` lowered to `{union_ty:?}`");
    };
    if !output.union_key_conformances.insert(mangled.clone()) {
        return;
    }
    let hash_symbol = mangled_method_name(mangled, &[], "hash", 1, &[]);
    let eq_symbol = mangled_method_name(mangled, &[], "equals?", 2, &[]);

    let hash = build_union_function(
        hash_symbol,
        &union_ty,
        1,
        IRType::Int64,
        registry,
        output,
        |args, ctx, block| {
            let subject = UnionSubject {
                members,
                ty: &union_ty,
                value: args[0],
            };
            emit_union_hash(subject, ctx, block)
        },
    );
    let eq = build_union_function(
        eq_symbol,
        &union_ty,
        2,
        IRType::Bool,
        registry,
        output,
        |args, ctx, block| lower_value_equality(args[0], args[1], &union_resolved, ctx, block),
    );
    output.synthesized_functions.push(hash);
    output.synthesized_functions.push(eq);
}

/// Hand-build one `Regular` function of `arity` params, all of
/// `param_ty`. Params promote into slots the same way a named fn's
/// do, `body` reads them back and produces the result, and the exit
/// drops release the slots. Mirrors `closures::build_fn_as_closure_wrapper`.
fn build_union_function(
    symbol: IRSymbol,
    param_ty: &IRType,
    arity: u32,
    return_type: IRType,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
    body: impl FnOnce(&[ValueId], &mut FnLowerCtx<'_>, IRBlockId) -> (ValueId, IRBlockId),
) -> IRFunction {
    let mut ctx = FnLowerCtx::new(registry, output);
    let entry = ctx.fresh_block("entry");
    let params: Vec<_> = (0..arity)
        .map(|index| {
            let local = IRLocalId::from_local_id(LocalId::new(index));
            promote_param(&mut ctx, entry, local, param_ty.clone())
        })
        .collect();
    let args = read_wrapper_args(&params, &mut ctx, entry);
    let (result, exit) = body(&args, &mut ctx, entry);
    emit_function_exit_drops(&mut ctx, exit);
    ctx.cfg.set_terminator(
        exit,
        IRTerminator::Return {
            value: Some(result),
        },
    );
    IRFunction {
        blocks: ctx.into_blocks(),
        def_location: None,
        kind: FunctionKind::Regular,
        params,
        return_type,
        symbol,
    }
}
