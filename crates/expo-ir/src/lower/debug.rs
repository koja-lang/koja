//! Lowering for the auto-synthesized `*_format` functions.
//!
//! Walks the type context to collect variant/field metadata and decides
//! the synthesized symbol name for a given enum or struct; emission then
//! builds the LLVM switch / GEP / snprintf scaffolding.

use expo_ast::identifier::TypeIdentifier;

use crate::lower::LowerCtx;
use crate::lower::naming::method_symbol_prefix;
use crate::resolved::debug::{
    ResolvedEnumFormatInfo, ResolvedFormatKind, ResolvedStructFormatInfo,
};

/// Resolves a bare type name to its [`TypeIdentifier`] via the type context.
pub fn resolve_type_id(ctx: &LowerCtx<'_>, name: &str) -> Result<TypeIdentifier, String> {
    ctx.type_ctx
        .resolve_name(name)
        .cloned()
        .ok_or_else(|| format!("no type identifier for `{name}`"))
}

/// Symbol name of the synthesized `format` function for a type. Mirrors
/// [`method_symbol_prefix`] so definition and call sites converge on the
/// same LLVM symbol (e.g. `debug_format.Color_format`).
pub fn format_fn_name(id: &TypeIdentifier) -> String {
    let prefix = method_symbol_prefix(&id.package, &id.name);
    format!("{prefix}_format")
}

/// Looks up variant metadata from the type context for enum format synthesis.
pub fn resolve_enum_format_info(ctx: &LowerCtx<'_>, id: &TypeIdentifier) -> ResolvedEnumFormatInfo {
    let variants = ctx
        .type_ctx
        .get_type(id)
        .and_then(|type_info| type_info.variants())
        .cloned()
        .unwrap_or_default();
    ResolvedEnumFormatInfo {
        function_name: format_fn_name(id),
        variants,
    }
}

/// Looks up field metadata from the type context for struct format synthesis.
pub fn resolve_struct_format_info(
    ctx: &LowerCtx<'_>,
    id: &TypeIdentifier,
) -> ResolvedStructFormatInfo {
    let fields = ctx
        .type_ctx
        .get_type(id)
        .and_then(|type_info| type_info.fields())
        .cloned()
        .unwrap_or_default();
    ResolvedStructFormatInfo {
        fields,
        function_name: format_fn_name(id),
    }
}

/// Determines the formatting strategy for a type by checking the type
/// context (enum vs struct) and the intrinsics table. The intrinsic
/// check is supplied as a callback so this helper stays free of any
/// backend-specific intrinsic registry.
pub fn resolve_format_kind(
    ctx: &LowerCtx<'_>,
    resolved_id: Option<&TypeIdentifier>,
    fn_name: &str,
    type_name: &str,
    is_primitive_intrinsic: impl Fn(&str) -> bool,
) -> Option<ResolvedFormatKind> {
    if let Some(id) = resolved_id
        && let Some(ti) = ctx.type_ctx.get_type(id)
    {
        if ti.is_enum() {
            return Some(ResolvedFormatKind::Enum);
        }
        if ti.is_struct() {
            return Some(ResolvedFormatKind::Struct);
        }
    } else {
        if ctx.type_ctx.is_enum(type_name) {
            return Some(ResolvedFormatKind::Enum);
        }
        if ctx.type_ctx.is_struct(type_name) {
            return Some(ResolvedFormatKind::Struct);
        }
    }
    if is_primitive_intrinsic(fn_name) {
        Some(ResolvedFormatKind::PrimitiveIntrinsic)
    } else {
        None
    }
}
