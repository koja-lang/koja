//! Lowering for `for` loops over `Enumeration`-implementing types.
//!
//! `for item in iterable` desugars at emission time to an indexed `while`
//! loop calling `iterable.length()` and `iterable.get(idx)`. To pick the
//! right impl methods and bind `item` with the right LLVM type, the
//! emitter needs the iterable's mangled type key, base name, type-args,
//! and element Expo type. [`resolve_enumerable_info`] computes all of
//! that against the type registry; emission then derives the LLVM
//! element type with one `to_llvm_type(...)` call.

use expo_typecheck::types::{Type, build_substitution, mangle_name, substitute_preserving};

use crate::lower::ctx::LowerCtx;
use crate::lower::mangling::try_parse_mangled_name;
use crate::lower::types::resolve_name_current;
use crate::resolved::loops::ResolvedEnumerable;

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

    let mangled_type = mangle_name(&base_id, &type_args);

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
