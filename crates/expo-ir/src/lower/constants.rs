//! Module-level constant lowering and the IR-side pool bridge.
//!
//! [`resolve_const_inline`] folds primitive literals; [`resolve_const`]
//! adds compound resolution. [`populate_constants`] runs once per
//! program, routing each constant to either
//! [`ConstantTables::primitives`] (inline [`IROperand`]) or
//! [`IRProgram::constants`] (pool-backed [`IRConstantValue`]).
//! [`ResolvedConst`] is fully consumed here -- backends never see it.

use std::collections::HashMap;

use expo_ast::ast::{EnumConstructionData, ExprKind, FieldInit, Item, Literal, Module, StringPart};
use expo_ast::identifier::TypeIdentifier;
use expo_typecheck::context::TypeContext;
use expo_typecheck::types::package_from_str;

use crate::constants::IRConstantValue;
use crate::identity::MonomorphizedTypeIdentifier;
use crate::lower::LowerCtx;
use crate::lower::types::resolve_name_current;
use crate::program::IRProgram;
use crate::resolved::constants::ResolvedConst;
use crate::util::parse_int_literal;
use crate::values::{IRConstId, IROperand};

/// Package-qualified lookup tables built by [`populate_constants`].
/// Both maps hold IR-native data so the lowerer translates once.
#[derive(Default)]
pub struct ConstantTables {
    /// Compounds (`String` / `EnumVariant` / `Struct`) -> pool id;
    /// the lowerer emits [`crate::IRInstruction::LoadConst`].
    pub compounds: HashMap<TypeIdentifier, IRConstId>,
    /// Primitives (`Bool` / `Float` / `Int`) -> inline operand;
    /// copied straight into the use site.
    pub primitives: HashMap<TypeIdentifier, IROperand>,
}

/// Resolve every module-level constant once, routing each to its
/// IR home: primitives become [`ConstantTables::primitives`]
/// operands, compounds become [`IRProgram::constants`] entries (with
/// the id mirrored into [`ConstantTables::compounds`]). Unfoldable
/// initializers are skipped.
pub fn populate_constants(
    modules: &[&Module],
    packages: &[&str],
    program: &mut IRProgram,
    type_ctx: &TypeContext,
    layouts: &crate::TypeLayouts,
) -> ConstantTables {
    let mut tables = ConstantTables::default();
    let empty_fn_lower = crate::FnLowerState::new();
    for (module, package) in modules.iter().zip(packages.iter()) {
        let pkg = package_from_str(package);
        let ctx = LowerCtx {
            closure_site_path: None,
            fn_lower: &empty_fn_lower,
            layouts,
            locals: &(),
            package: Some(&pkg),
            type_ctx,
        };
        for item in &module.items {
            let Item::Constant(constant) = item else {
                continue;
            };
            let Some(value) = resolve_const(&constant.value.kind, &ctx) else {
                continue;
            };
            let const_id = TypeIdentifier {
                package: pkg.clone(),
                name: constant.name.clone(),
            };
            match value {
                ResolvedConst::Bool(_) | ResolvedConst::Float(_) | ResolvedConst::Int(_) => {
                    tables
                        .primitives
                        .insert(const_id, resolved_to_operand(&value));
                }
                ResolvedConst::String(s) => {
                    let id = program.push_constant(const_id.clone(), IRConstantValue::String(s));
                    tables.compounds.insert(const_id, id);
                }
                ResolvedConst::EnumVariant {
                    enum_id,
                    variant,
                    tag,
                } => {
                    let id = program.push_constant(
                        const_id.clone(),
                        IRConstantValue::EnumVariant {
                            enum_id,
                            variant,
                            tag,
                        },
                    );
                    tables.compounds.insert(const_id, id);
                }
                ResolvedConst::Struct { struct_id, fields } => {
                    let operand_fields = fields
                        .into_iter()
                        .map(|(name, value)| (name, resolved_to_operand(&value)))
                        .collect();
                    let id = program.push_constant(
                        const_id.clone(),
                        IRConstantValue::Struct {
                            struct_id,
                            fields: operand_fields,
                        },
                    );
                    tables.compounds.insert(const_id, id);
                }
            }
        }
    }
    tables
}

/// Fold the primitive arms (`Bool` / `Float` / `Int` / pure
/// `String`); returns `None` on any compound shape. Used where no
/// [`LowerCtx`] is in scope, and for struct-field values (which
/// can't be compounds anyway).
pub fn resolve_const_inline(kind: &ExprKind) -> Option<ResolvedConst> {
    match kind {
        ExprKind::Literal {
            value: Literal::Bool(b),
        } => Some(ResolvedConst::Bool(*b)),
        ExprKind::Literal {
            value: Literal::Float(s),
        } => s.parse().ok().map(ResolvedConst::Float),
        ExprKind::Literal {
            value: Literal::Int(s),
        } => parse_int_literal(s).ok().map(ResolvedConst::Int),
        ExprKind::String { parts, .. } => fold_string_literal(parts).map(ResolvedConst::String),
        _ => None,
    }
}

/// Primitives via [`resolve_const_inline`], plus compound shapes
/// (`EnumVariant` tag lookup, `Struct` field destructure) via `ctx`.
pub fn resolve_const(kind: &ExprKind, ctx: &LowerCtx<'_>) -> Option<ResolvedConst> {
    if let Some(inline) = resolve_const_inline(kind) {
        return Some(inline);
    }
    match kind {
        ExprKind::EnumConstruction {
            type_path,
            variant,
            data: EnumConstructionData::Unit,
            ..
        } => {
            let enum_name = type_path.join(".");
            let enum_id = resolve_name_current(ctx, &enum_name).cloned()?;
            let tag = ctx.layouts.variant_index(
                &MonomorphizedTypeIdentifier::new(enum_id.qualified_name()),
                variant,
            )?;
            Some(ResolvedConst::EnumVariant {
                enum_id,
                variant: variant.clone(),
                tag,
            })
        }
        ExprKind::StructConstruction {
            type_path, fields, ..
        } => {
            let struct_name = type_path.join(".");
            let struct_id = resolve_name_current(ctx, &struct_name).cloned()?;
            let resolved_fields = resolve_const_struct_fields(fields)?;
            Some(ResolvedConst::Struct {
                struct_id,
                fields: resolved_fields,
            })
        }
        _ => None,
    }
}

/// Fold each user-supplied field with [`resolve_const_inline`].
/// Returns `None` on any non-literal field. Partial initializers are
/// rejected by typecheck upstream, so user order matches declared
/// order.
fn resolve_const_struct_fields(user_inits: &[FieldInit]) -> Option<Vec<(String, ResolvedConst)>> {
    user_inits
        .iter()
        .map(|init| Some((init.name.clone(), resolve_const_inline(&init.value.kind)?)))
        .collect()
}

/// Primitive [`ResolvedConst`] -> inline [`IROperand`]. Panics on
/// compounds; callers must route those through the pool first.
pub(crate) fn resolved_to_operand(value: &ResolvedConst) -> IROperand {
    match value {
        ResolvedConst::Bool(b) => IROperand::ConstBool(*b),
        ResolvedConst::Float(f) => IROperand::ConstFloat(*f),
        ResolvedConst::Int(n) => IROperand::ConstInt(*n),
        ResolvedConst::String(s) => IROperand::ConstStr(s.clone()),
        ResolvedConst::EnumVariant { .. } | ResolvedConst::Struct { .. } => {
            unreachable!("resolved_to_operand called on a compound ResolvedConst")
        }
    }
}

fn fold_string_literal(parts: &[StringPart]) -> Option<String> {
    let mut combined = String::new();
    for part in parts {
        match part {
            StringPart::Literal { value, .. } => combined.push_str(value),
            // Interpolated parts mean the string isn't a compile-time
            // constant; bail so the operand path falls back to the
            // AST-driven `compile_string` route.
            _ => return None,
        }
    }
    Some(combined)
}
