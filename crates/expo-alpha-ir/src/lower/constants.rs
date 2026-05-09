//! Package-level `const` pool lowering and registry → [`IRConstantValue`] translation.
//! Primitives inline at use sites as [`IRInstruction::Const`](crate::function::IRInstruction::Const);
//! strings, unit enum variants, and struct literals pool on
//! [`IRPackage::constants`](crate::package::IRPackage::constants) and load through
//! [`IRInstruction::LoadConst`](crate::function::IRInstruction::LoadConst).

use expo_alpha_typecheck::{GlobalKind, GlobalRegistry};
use expo_ast::ast::{Constant, Expr, ExprKind, Literal, StringPart, UnaryOp};
use expo_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};

use crate::constant::IRConstantValue;
use crate::enum_decl::IRVariantTag;
use crate::function::IRSymbol;
use crate::types::ConstValue;

/// Translate a top-level `const NAME = <rhs>` into a pool entry, or
/// `None` for primitives (which inline at use sites).
pub(super) fn lower_constant_pool_entry(
    constant: &Constant,
    package: &str,
    registry: &GlobalRegistry,
) -> Option<(IRSymbol, IRConstantValue)> {
    let identifier = Identifier::new(package, vec![constant.name.clone()]);
    let (id, entry) = registry.lookup(&identifier)?;
    if !matches!(entry.kind, GlobalKind::Constant(Some(_))) {
        return None;
    }
    let value = constant_value_from_registry(id, registry)?;
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
) -> Option<IRConstantValue> {
    let entry = registry.get(id)?;
    let GlobalKind::Constant(Some(def)) = &entry.kind else {
        return None;
    };
    lower_constant_value(&def.value, registry)
}

/// Recursively translate an already-resolved constant `Expr` into an
/// [`IRConstantValue`]. Returns `None` on shapes the lift pass should
/// have rejected — IR treats those as compiler bugs (the typecheck
/// seal would have caught them otherwise).
fn lower_constant_value(expr: &Expr, registry: &GlobalRegistry) -> Option<IRConstantValue> {
    match &expr.kind {
        ExprKind::Group { expr: inner } => lower_constant_value(inner, registry),
        ExprKind::Literal { value } => Some(IRConstantValue::Primitive(literal_to_const(value))),
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
            let inner = lower_constant_value(operand, registry)?;
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
                let value = lower_constant_value(&init.value, registry)?;
                canonical[index as usize] = Some(value);
            }
            let fields = canonical.into_iter().collect::<Option<Vec<_>>>()?;
            let symbol = IRSymbol::from_identifier(&entry.identifier);
            Some(IRConstantValue::Struct { fields, ty: symbol })
        }
        _ => None,
    }
}

fn literal_to_const(value: &Literal) -> ConstValue {
    match value {
        Literal::Bool(b) => ConstValue::Bool(*b),
        Literal::Float(text) => ConstValue::Float64(text.parse().unwrap_or(0.0)),
        Literal::Int(text) => ConstValue::Int64(text.parse().unwrap_or(0)),
        Literal::String(s) => ConstValue::String(s.clone()),
        Literal::Unit => ConstValue::Unit,
    }
}

fn negate_primitive(value: IRConstantValue) -> Option<IRConstantValue> {
    match value {
        IRConstantValue::Primitive(ConstValue::Int64(n)) => {
            Some(IRConstantValue::Primitive(ConstValue::Int64(-n)))
        }
        IRConstantValue::Primitive(ConstValue::Float64(f)) => {
            Some(IRConstantValue::Primitive(ConstValue::Float64(-f)))
        }
        _ => None,
    }
}
