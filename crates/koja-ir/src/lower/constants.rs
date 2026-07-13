//! Package-level `const` pool lowering and registry -> [`IRConstantValue`] translation.
//! Primitives inline at use sites as [`IRInstruction::Const`](crate::function::IRInstruction::Const).
//! Strings, binaries, unit enum variants, and struct literals pool on
//! [`IRPackage::constants`](crate::package::IRPackage::constants) and load through
//! [`IRInstruction::LoadConst`](crate::function::IRInstruction::LoadConst).

use koja_ast::ast::{BinarySegment, Constant, Expr, ExprKind, Literal, StringPart, UnaryOp};
use koja_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};
use koja_typecheck::{GlobalKind, GlobalRegistry, LiteralCoercion, NumericLiteralWidth};

use crate::binary_packing::pack_integer_segment;
use crate::constant::IRConstantValue;
use crate::enum_decl::IRVariantTag;
use crate::function::IRSymbol;
use crate::types::ConstValue;

use super::binary_literal::{ClassifiedSegment, ast_endianness_to_ir, classify_segment};
use super::ops::{int_const_at_width, parse_int_literal};

/// Translate a top-level `const NAME = <rhs>` into a pool entry, or
/// `None` for primitives (which inline at use sites). Each
/// `Expr.literal_coercion` annotation drives the matching narrow
/// `ConstValue::*` head — e.g. `const PI: Float32 = 3.14` lowers
/// as `ConstValue::Float32` rather than the default 64-bit form.
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
/// [`IRInstruction::Const`](crate::function::IRInstruction::Const)). Heap payloads (strings, binaries,
/// bits), unit enum variants, and structs of literals pool. Scalar
/// numeric / bool / unit primitives inline. Mirrors v1's
/// `ConstantTables` admission rule, where the binary size win is on
/// compound constants rather than primitives that fit in a register.
pub(super) fn pools_in_constant_pool(value: &IRConstantValue) -> bool {
    match value {
        IRConstantValue::EnumVariant { .. } | IRConstantValue::Struct { .. } => true,
        IRConstantValue::Primitive(
            ConstValue::Binary(_) | ConstValue::Bits { .. } | ConstValue::String(_),
        ) => true,
        IRConstantValue::Primitive(_) => false,
    }
}

/// Walk the registry's stamped [`ConstantDefinition`](koja_typecheck::registry::ConstantDefinition) for `id` into
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
/// seal would have caught them otherwise). Each subexpression's
/// `literal_coercion` annotation drives the resulting `ConstValue::*`
/// width; absent annotation, primitives keep their default 64-bit
/// head.
fn lower_constant_value(expr: &Expr, registry: &GlobalRegistry) -> Option<IRConstantValue> {
    match &expr.kind {
        ExprKind::Group { expr: inner } => lower_constant_value(inner, registry),
        ExprKind::Literal { value } => Some(IRConstantValue::Primitive(literal_to_const(
            value,
            literal_width(expr),
        ))),
        ExprKind::String { parts, .. } => {
            let [StringPart::Literal { value, .. }] = parts.as_slice() else {
                return None;
            };
            Some(IRConstantValue::Primitive(ConstValue::String(
                value.clone(),
            )))
        }
        ExprKind::BinaryLiteral { segments } => fold_binary_literal(segments),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => {
            // Outer-stamp coercion wins: fold the negation into a
            // single typed `Const` at the recorded width. Falls back
            // to the recursive path (operand inherits its own
            // annotation) when the outer is unstamped.
            if let Some(target) = literal_width(expr)
                && let Some(folded) = fold_negated_literal(operand, target)
            {
                return Some(IRConstantValue::Primitive(folded));
            }
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

/// Fold an all-literal `<<...>>` constant into its packed bytes.
/// Typecheck restricted every segment value to a literal, so the
/// fold reuses the runtime classifier and the shared bit packer to
/// stay byte-identical with a runtime construction of the same
/// literal. Returns `None` on any shape the lift should have
/// rejected.
fn fold_binary_literal(segments: &[BinarySegment]) -> Option<IRConstantValue> {
    let mut scratch_diagnostics = Vec::new();
    let mut classified = Vec::with_capacity(segments.len());
    let mut total_bits: u64 = 0;
    for segment in segments {
        let kind = classify_segment(segment, segment.span, &mut scratch_diagnostics).ok()?;
        let width = match &kind {
            ClassifiedSegment::Integer { width } | ClassifiedSegment::Float { width } => *width,
            ClassifiedSegment::String { byte_length } => byte_length * 8,
        };
        classified.push((kind, total_bits));
        total_bits += width;
    }

    let mut buffer = vec![0u8; total_bits.div_ceil(8) as usize];
    for (segment, (kind, bit_offset)) in segments.iter().zip(classified) {
        let endian = ast_endianness_to_ir(segment.endianness);
        match kind {
            ClassifiedSegment::Integer { width } => {
                let bits = segment_int_bits(&segment.value)?;
                pack_integer_segment(&mut buffer, bits, width, endian, bit_offset);
            }
            ClassifiedSegment::Float { width } => {
                let bits = segment_float_bits(&segment.value, width)?;
                pack_integer_segment(&mut buffer, bits, width, endian, bit_offset);
            }
            ClassifiedSegment::String { byte_length } => {
                let bytes = segment_string_bytes(&segment.value)?;
                let start = (bit_offset / 8) as usize;
                buffer[start..start + byte_length as usize].copy_from_slice(&bytes);
            }
        }
    }

    if total_bits.is_multiple_of(8) {
        Some(IRConstantValue::Primitive(ConstValue::Binary(buffer)))
    } else {
        Some(IRConstantValue::Primitive(ConstValue::Bits {
            bit_length: total_bits,
            bytes: buffer,
        }))
    }
}

fn segment_int_bits(value: &Expr) -> Option<u64> {
    let ExprKind::Literal {
        value: Literal::Int(text),
    } = &value.kind
    else {
        return None;
    };
    parse_int_literal(text).ok().map(|parsed| parsed as u64)
}

fn segment_float_bits(value: &Expr, width: u64) -> Option<u64> {
    let ExprKind::Literal {
        value: Literal::Float(text),
    } = &value.kind
    else {
        return None;
    };
    let parsed = text.parse::<f64>().ok()?;
    Some(match width {
        32 => u64::from((parsed as f32).to_bits()),
        _ => parsed.to_bits(),
    })
}

fn segment_string_bytes(value: &Expr) -> Option<Vec<u8>> {
    let ExprKind::String { parts, .. } = &value.kind else {
        return None;
    };
    let mut bytes = Vec::new();
    for part in parts {
        let StringPart::Literal { value, .. } = part else {
            return None;
        };
        bytes.extend_from_slice(value.as_bytes());
    }
    Some(bytes)
}

/// Pull the typecheck-stamped numeric width off `expr`'s
/// `literal_coercion` slot, when present. Reserved for the
/// constant-pool sites that emit a typed `Const` opcode.
fn literal_width(expr: &Expr) -> Option<NumericLiteralWidth> {
    expr.literal_coercion
        .as_ref()
        .and_then(LiteralCoercion::numeric_width)
}

fn literal_to_const(value: &Literal, target: Option<NumericLiteralWidth>) -> ConstValue {
    match value {
        Literal::Bool(b) => ConstValue::Bool(*b),
        Literal::Float(text) => match (text.parse::<f64>(), target) {
            (Ok(parsed), Some(NumericLiteralWidth::Float32)) => ConstValue::Float32(parsed as f32),
            (Ok(parsed), _) => ConstValue::Float64(parsed),
            (Err(_), _) => ConstValue::Float64(0.0),
        },
        Literal::Int(text) => match parse_int_literal(text) {
            Ok(parsed) => int_const_at_width(parsed, target),
            Err(_) => ConstValue::Int64(0),
        },
        Literal::String(s) => ConstValue::String(s.clone()),
        Literal::Unit => ConstValue::Unit,
    }
}

/// Fold a `UnaryOp::Neg(Literal::Int | Literal::Float)` whose outer
/// `literal_coercion` is stamped directly to a single typed
/// `ConstValue` at the recorded width, with the negation applied
/// at fold time. Returns `None` for shapes the typecheck pass would
/// never have annotated (non-literal operand, group-wrapped
/// non-literal, etc.) — caller falls back to the regular recursive
/// path.
fn fold_negated_literal(operand: &Expr, target: NumericLiteralWidth) -> Option<ConstValue> {
    match &operand.kind {
        ExprKind::Group { expr } => fold_negated_literal_inner(expr, target),
        _ => fold_negated_literal_inner(operand, target),
    }
}

fn fold_negated_literal_inner(operand: &Expr, target: NumericLiteralWidth) -> Option<ConstValue> {
    match &operand.kind {
        ExprKind::Literal {
            value: Literal::Int(text),
        } => parse_int_literal(text)
            .ok()
            .and_then(i128::checked_neg)
            .map(|neg| int_const_at_width(neg, Some(target))),
        ExprKind::Literal {
            value: Literal::Float(text),
        } => text.parse::<f64>().ok().map(|f| match target {
            NumericLiteralWidth::Float32 => ConstValue::Float32(-f as f32),
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
