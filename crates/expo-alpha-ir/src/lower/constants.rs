//! Package-level `const` pool lowering and registry → [`IRConstantValue`] translation.
//! Primitives inline at use sites as [`IRInstruction::Const`](crate::function::IRInstruction::Const);
//! strings, unit enum variants, and struct literals pool on
//! [`IRPackage::constants`](crate::package::IRPackage::constants) and load through
//! [`IRInstruction::LoadConst`](crate::function::IRInstruction::LoadConst).

use expo_alpha_typecheck::{Coercions, GlobalKind, GlobalRegistry};
use expo_ast::ast::{Constant, Expr, ExprKind, Literal, StringPart, UnaryOp};
use expo_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};

use crate::constant::IRConstantValue;
use crate::enum_decl::IRVariantTag;
use crate::function::IRSymbol;
use crate::types::ConstValue;

use super::ops::{int_const_at_width, parse_int_literal};

/// Translate a top-level `const NAME = <rhs>` into a pool entry, or
/// `None` for primitives (which inline at use sites). Reads
/// `coercions` so a `const PI: Float32 = 3.14` rhs lowers as
/// `ConstValue::Float32` rather than the default 64-bit form.
pub(super) fn lower_constant_pool_entry(
    constant: &Constant,
    package: &str,
    registry: &GlobalRegistry,
    coercions: &Coercions,
) -> Option<(IRSymbol, IRConstantValue)> {
    let identifier = Identifier::new(package, vec![constant.name.clone()]);
    let (id, entry) = registry.lookup(&identifier)?;
    if !matches!(entry.kind, GlobalKind::Constant(Some(_))) {
        return None;
    }
    let value = constant_value_from_registry(id, registry, coercions)?;
    if !pools_in_constant_pool(&value) {
        return None;
    }
    Some((IRSymbol::from_identifier(&entry.identifier), value))
}

/// True when an [`IRConstantValue`] should live in the package
/// constant pool (vs. inlining at the use site as
/// [`IRInstruction::Const`](crate::function::IRInstruction::Const)). Strings, unit enum variants, and
/// structs of literals pool; scalar numeric / bool / unit primitives
/// inline. Mirrors v1's `ConstantTables` admission rule — the binary
/// size win is on compound constants, not on primitives that fit in
/// a register.
pub(super) fn pools_in_constant_pool(value: &IRConstantValue) -> bool {
    match value {
        IRConstantValue::EnumVariant { .. } | IRConstantValue::Struct { .. } => true,
        IRConstantValue::Primitive(ConstValue::String(_)) => true,
        IRConstantValue::Primitive(_) => false,
    }
}

/// Walk the registry's stamped [`ConstantDefinition`](expo_alpha_typecheck::registry::ConstantDefinition) for `id` into
/// an [`IRConstantValue`]. Reads the stamped definition rather than
/// any AST `Constant.value` — both are correct, but the registry
/// copy is what IR considers authoritative (the AST may be
/// substituted at monomorphization time; the registry is not).
pub(super) fn constant_value_from_registry(
    id: GlobalRegistryId,
    registry: &GlobalRegistry,
    coercions: &Coercions,
) -> Option<IRConstantValue> {
    let entry = registry.get(id)?;
    let GlobalKind::Constant(Some(def)) = &entry.kind else {
        return None;
    };
    lower_constant_value(&def.value, registry, coercions)
}

/// Recursively translate an already-resolved constant `Expr` into an
/// [`IRConstantValue`]. Returns `None` on shapes the lift pass should
/// have rejected — IR treats those as compiler bugs (the typecheck
/// seal would have caught them otherwise). `coercions` lets a
/// recorded narrow target (e.g. `const FD: UInt8 = 1`) materialize
/// at the right `ConstValue::*` width; absent a record, primitives
/// keep their default 64-bit head.
fn lower_constant_value(
    expr: &Expr,
    registry: &GlobalRegistry,
    coercions: &Coercions,
) -> Option<IRConstantValue> {
    match &expr.kind {
        ExprKind::Group { expr: inner } => lower_constant_value(inner, registry, coercions),
        ExprKind::Literal { value } => {
            let target = coercions.get(&expr.span).copied();
            Some(IRConstantValue::Primitive(literal_to_const(value, target)))
        }
        ExprKind::String { parts, .. } => {
            let [StringPart::Literal { value, .. }] = parts.as_slice() else {
                return None;
            };
            Some(IRConstantValue::Primitive(ConstValue::String(
                value.clone(),
            )))
        }
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => {
            // Recorded coercion on the outer `Unary` wins: fold the
            // negation into a single typed `Const` at the recorded
            // width. Falls back to the recursive path (operand
            // inherits its own coercion record) when nothing's
            // recorded outside.
            if let Some(target) = coercions.get(&expr.span).copied()
                && let Some(folded) = fold_negated_literal(operand, target)
            {
                return Some(IRConstantValue::Primitive(folded));
            }
            let inner = lower_constant_value(operand, registry, coercions)?;
            negate_primitive(inner)
        }
        ExprKind::EnumConstruction { variant, .. } => {
            let ResolvedType::Named {
                resolution: Resolution::Global(enum_id),
                ..
            } = expr.resolution
            else {
                return None;
            };
            let entry = registry.get(enum_id)?;
            let GlobalKind::Enum(Some(enum_def)) = &entry.kind else {
                return None;
            };
            let (tag, _) = enum_def.lookup_variant(variant)?;
            let symbol = IRSymbol::from_identifier(&entry.identifier);
            Some(IRConstantValue::EnumVariant {
                tag: IRVariantTag(tag as u8),
                ty: symbol,
            })
        }
        ExprKind::StructConstruction { fields, .. } => {
            let ResolvedType::Named {
                resolution: Resolution::Global(struct_id),
                ..
            } = expr.resolution
            else {
                return None;
            };
            let entry = registry.get(struct_id)?;
            let GlobalKind::Struct(Some(struct_def)) = &entry.kind else {
                return None;
            };
            let mut canonical: Vec<Option<IRConstantValue>> = vec![None; struct_def.fields.len()];
            for init in fields {
                let (index, _) = struct_def.lookup_field(&init.name)?;
                let value = lower_constant_value(&init.value, registry, coercions)?;
                canonical[index as usize] = Some(value);
            }
            let fields = canonical.into_iter().collect::<Option<Vec<_>>>()?;
            let symbol = IRSymbol::from_identifier(&entry.identifier);
            Some(IRConstantValue::Struct { fields, ty: symbol })
        }
        _ => None,
    }
}

fn literal_to_const(
    value: &Literal,
    target: Option<expo_alpha_typecheck::NumericLiteralWidth>,
) -> ConstValue {
    match value {
        Literal::Bool(b) => ConstValue::Bool(*b),
        Literal::Float(text) => match (text.parse::<f64>(), target) {
            (Ok(parsed), Some(expo_alpha_typecheck::NumericLiteralWidth::Float32)) => {
                ConstValue::Float32(parsed as f32)
            }
            (Ok(parsed), _) => ConstValue::Float64(parsed),
            (Err(_), _) => ConstValue::Float64(0.0),
        },
        Literal::Int(text) => match parse_int_literal(text) {
            Ok(parsed) => int_const_at_width(parsed as i128, target),
            Err(_) => ConstValue::Int64(0),
        },
        Literal::String(s) => ConstValue::String(s.clone()),
        Literal::Unit => ConstValue::Unit,
    }
}

/// Fold a `UnaryOp::Neg(Literal::Int | Literal::Float)` whose outer
/// span is registered in the coercion table directly to a single
/// typed `ConstValue` at the recorded width, with the negation
/// applied at fold time. Returns `None` for shapes the typecheck
/// pass would never have stamped a coercion on (non-literal
/// operand, group-wrapped non-literal, etc.) — caller falls back
/// to the regular recursive path.
fn fold_negated_literal(
    operand: &Expr,
    target: expo_alpha_typecheck::NumericLiteralWidth,
) -> Option<ConstValue> {
    match &operand.kind {
        ExprKind::Group { expr } => fold_negated_literal_inner(expr, target),
        _ => fold_negated_literal_inner(operand, target),
    }
}

fn fold_negated_literal_inner(
    operand: &Expr,
    target: expo_alpha_typecheck::NumericLiteralWidth,
) -> Option<ConstValue> {
    match &operand.kind {
        ExprKind::Literal {
            value: Literal::Int(text),
        } => parse_int_literal(text)
            .ok()
            .and_then(|n| (n as i128).checked_neg())
            .map(|neg| int_const_at_width(neg, Some(target))),
        ExprKind::Literal {
            value: Literal::Float(text),
        } => text.parse::<f64>().ok().map(|f| match target {
            expo_alpha_typecheck::NumericLiteralWidth::Float32 => ConstValue::Float32(-f as f32),
            _ => ConstValue::Float64(-f),
        }),
        ExprKind::Group { expr } => fold_negated_literal_inner(expr, target),
        _ => None,
    }
}

fn negate_primitive(value: IRConstantValue) -> Option<IRConstantValue> {
    match value {
        IRConstantValue::Primitive(ConstValue::Int8(n)) => {
            Some(IRConstantValue::Primitive(ConstValue::Int8(-n)))
        }
        IRConstantValue::Primitive(ConstValue::Int16(n)) => {
            Some(IRConstantValue::Primitive(ConstValue::Int16(-n)))
        }
        IRConstantValue::Primitive(ConstValue::Int32(n)) => {
            Some(IRConstantValue::Primitive(ConstValue::Int32(-n)))
        }
        IRConstantValue::Primitive(ConstValue::Int64(n)) => {
            Some(IRConstantValue::Primitive(ConstValue::Int64(-n)))
        }
        IRConstantValue::Primitive(ConstValue::Float32(f)) => {
            Some(IRConstantValue::Primitive(ConstValue::Float32(-f)))
        }
        IRConstantValue::Primitive(ConstValue::Float64(f)) => {
            Some(IRConstantValue::Primitive(ConstValue::Float64(-f)))
        }
        _ => None,
    }
}
