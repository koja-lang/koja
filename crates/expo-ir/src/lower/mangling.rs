//! Lowering helpers for parsing mangled generic type names back into a
//! `(base, type_args)` pair.
//!
//! Codegen produces names like `Pair_$Int.String$` when monomorphizing
//! generics; many lowering decisions need to recover the base name and
//! type arguments to look up variants, fields, or the underlying generic
//! AST. The chain is fully pure-semantic — only the type context's
//! generic-AST tables and package index are consulted — so it lives here
//! beside the rest of the type-resolution helpers.

use std::mem;

use expo_typecheck::types::{Primitive, Type, named, named_generic};

use crate::lower::ctx::LowerCtx;
use crate::lower::types::resolve_name_current;

/// Attempts to recover the base name and concrete type args from a mangled
/// name like `Pair_$Int.String$`. Returns `None` if the name doesn't match
/// a known generic struct or enum template.
pub fn try_parse_mangled_name(ctx: &LowerCtx<'_>, mangled: &str) -> Option<(String, Vec<Type>)> {
    let sep_pos = mangled.find("_$")?;
    let base = &mangled[..sep_pos];
    if !ctx.type_ctx.generic_struct_asts.contains_key(base)
        && !ctx.type_ctx.generic_enum_asts.contains_key(base)
    {
        return None;
    }
    if !mangled.ends_with('$') {
        return None;
    }
    let inner = &mangled[sep_pos + 2..mangled.len() - 1];
    let parts = split_mangled_args(ctx, inner);
    let type_args: Vec<Type> = parts.iter().map(|s| parse_mangled_type(ctx, s)).collect();
    Some((base.to_string(), type_args))
}

/// Splits a mangled args string on `.` at depth 0, respecting nested
/// `_$...$`. Because user-package type names use `.` as a package-qualifier
/// separator (`http.Header`) and mangled args use `.` as a delimiter, a
/// naive split would turn `Option_$http.Header$` into two args `http` and
/// `Header`. We resolve the ambiguity by preferring package-qualified
/// types: a `.` split is treated as a package boundary (not an arg
/// delimiter) when the token before the `.` names a known package and the
/// token after resolves to a type in that package.
fn split_mangled_args(ctx: &LowerCtx<'_>, s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'_' && bytes[i + 1] == b'$' {
            depth += 1;
            current.push('_');
            current.push('$');
            i += 2;
        } else if bytes[i] == b'$' {
            depth -= 1;
            current.push('$');
            i += 1;
        } else if bytes[i] == b'.' && depth == 0 {
            let rest = &s[i + 1..];
            let next_token = next_mangled_token(rest);
            if ctx.type_ctx.has_named_package(&current)
                && is_type_in_package(ctx, &current, next_token)
            {
                current.push('.');
                i += 1;
            } else {
                parts.push(mem::take(&mut current));
                i += 1;
            }
        } else {
            current.push(bytes[i] as char);
            i += 1;
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Returns the substring up to the next depth-0 `.` or end of string. Used
/// by [`split_mangled_args`] to peek the token following a candidate split
/// point so we can decide whether `.` is a package separator or an arg
/// delimiter.
fn next_mangled_token(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'_' && bytes[i + 1] == b'$' {
            depth += 1;
            i += 2;
        } else if bytes[i] == b'$' {
            depth = depth.saturating_sub(1);
            i += 1;
        } else if bytes[i] == b'.' && depth == 0 {
            return &s[..i];
        } else {
            i += 1;
        }
    }
    s
}

fn is_type_in_package(ctx: &LowerCtx<'_>, pkg: &str, name: &str) -> bool {
    let bare = name.split_once("_$").map(|(b, _)| b).unwrap_or(name);
    ctx.type_ctx.has_type_in_named_package(pkg, bare)
}

fn parse_mangled_type(ctx: &LowerCtx<'_>, s: &str) -> Type {
    if s == "unit" {
        return Type::Unit;
    }
    if let Some(p) = Primitive::from_name(s) {
        return Type::Primitive(p);
    }
    if let Some((base, args)) = try_parse_mangled_name(ctx, s) {
        return if let Some(id) = resolve_name_current(ctx, &base) {
            Type::Named {
                identifier: id.clone(),
                type_args: args,
            }
        } else {
            named_generic(&base, args, ctx.type_ctx, ctx.package)
        };
    }
    if let Some(id) = resolve_name_current(ctx, s) {
        return Type::Named {
            identifier: id.clone(),
            type_args: vec![],
        };
    }
    named(s)
}
