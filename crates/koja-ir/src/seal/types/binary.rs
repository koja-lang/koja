//! Operand types for the binary family: `BinaryConstruct` and
//! `BinaryMatch`.

use crate::seal::seal_panic;
use crate::types::{IRType, LoweredBinaryPattern, LoweredBinarySegment, ValueId};

use super::{TypeScope, require_one_of};

/// Each segment value must carry the type its segment kind expects.
pub(super) fn seal_binary_construct(segments: &[LoweredBinarySegment], scope: &TypeScope<'_>) {
    for segment in segments {
        let (value, valid) = match segment {
            LoweredBinarySegment::Float { value, .. } => {
                (*value, scope.value_type(*value).is_float())
            }
            LoweredBinarySegment::Integer { value, .. } => {
                (*value, scope.value_type(*value).is_int())
            }
            LoweredBinarySegment::String { value, .. } => {
                (*value, scope.value_type(*value) == &IRType::String)
            }
        };
        if !valid {
            seal_panic(&format!(
                "{} binary segment value `{value}` has incompatible type `{:?}`",
                scope.owner,
                scope.value_type(value)
            ));
        }
    }
}

/// The subject must be `Binary` or `Bits`, and each binding segment
/// must name a local declared with the segment's type.
pub(super) fn seal_binary_match(
    patterns: &[LoweredBinaryPattern],
    subject: ValueId,
    scope: &TypeScope<'_>,
) {
    require_one_of(
        scope.value_type(subject),
        &[IRType::Binary, IRType::Bits],
        &format!("{} BinaryMatch subject `{subject}`", scope.owner),
    );
    seal_binary_pattern_locals(patterns, scope);
}

fn seal_binary_pattern_locals(patterns: &[LoweredBinaryPattern], scope: &TypeScope<'_>) {
    for pattern in patterns {
        match pattern {
            LoweredBinaryPattern::BindInt { local, ty, .. } => {
                scope.require_local_type(*local, ty, "BinaryMatch binding");
            }
            LoweredBinaryPattern::GreedyTail {
                local: Some(local),
                ty,
                ..
            } => {
                scope.require_local_type(*local, ty, "BinaryMatch tail");
            }
            LoweredBinaryPattern::Discard { .. }
            | LoweredBinaryPattern::GreedyTail { local: None, .. }
            | LoweredBinaryPattern::LiteralBytes { .. }
            | LoweredBinaryPattern::LiteralInt { .. } => {}
        }
    }
}
