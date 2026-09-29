//! Emit-time verification of the wire-byte enum contracts.
//!
//! The native runtime stamps `Process.ExitReason`, `Process.Lifecycle`,
//! and `IO.Ready` wire bytes straight into envelope payloads, and the
//! emitted receive code loads those bytes directly as enum tags (zero
//! copy). The `Ref.call` / `Ref.cast` envelope packers write `Option`
//! tag words the same way. Those paths only work while wire byte ==
//! declared tag, so this check turns a stdlib variant reorder into a
//! build failure instead of silent value corruption.
//!
//! The same check covers `Process.Context`. `Process.context` and
//! `TraceRuntime.install` move the whole struct through one pointer as
//! raw bytes, so the Koja declaration must keep the word count and
//! widths the runtime's `Context` has.
//!
//! The tables mirror ABI.md by spec, not via a shared runtime type,
//! the same policy as `ReceiveTag::wire_byte` (see
//! `koja-runtime-core`'s `wire` module doc). The Rust side of each
//! contract is pinned by unit tests in `koja-runtime-core`.

use koja_ir::{IREnumVariant, IRType};

use crate::ctx::EmitContext;
use crate::error::LlvmError;

/// Wire-ordered variant names per ABI.md's envelope catalog.
const WIRE_ORDERED_ENUMS: &[(&str, &[&str])] = &[
    ("Global.IO.Ready", &["Read", "Write", "Error"]),
    (
        "Global.Process.ExitReason",
        &["Normal", "Shutdown", "Killed", "Crashed"],
    ),
    (
        "Global.Process.Lifecycle",
        &["Shutdown", "Interrupt", "Reload"],
    ),
];

/// Wire-ordered `Option` variant names. Checked against every
/// monomorphized instantiation because the envelope packers stamp the
/// tag word without a symbol in hand.
const OPTION_WIRE_ORDER: &[&str] = &["Some", "None"];

/// Mangled-name prefix every monomorphized `Option` symbol carries.
const OPTION_SYMBOL_PREFIX: &str = "Global.Option_$";

/// Field types of the structs the runtime copies as raw bytes, per
/// ABI.md's context layout. Names and meaning are pinned by the stdlib
/// tests, this table pins the byte layout.
const WIRE_STRUCT_FIELDS: &[(&str, &[IRType])] = &[(
    "Global.Process.Context",
    &[IRType::Int64, IRType::Int64, IRType::Int64, IRType::Int64],
)];

/// Verify every registered wire-coupled enum declares its variants in
/// the ABI.md wire order. Runs once per compile, after enum
/// registration. Skips enums the program never instantiated: absent
/// from the binary means no wire coupling to protect.
pub(crate) fn assert_wire_enum_order(ctx: &EmitContext<'_>) -> Result<(), LlvmError> {
    let mut violation = None;
    ctx.layouts.for_each_enum(|symbol, variants| {
        if violation.is_some() {
            return;
        }
        let mangled = symbol.mangled();
        let expected = if mangled.starts_with(OPTION_SYMBOL_PREFIX) {
            OPTION_WIRE_ORDER
        } else {
            match WIRE_ORDERED_ENUMS.iter().find(|(name, _)| *name == mangled) {
                Some((_, expected)) => expected,
                None => return,
            }
        };
        if let Some(mismatch) = wire_order_mismatch(variants, expected) {
            violation = Some(format!(
                "enum `{mangled}` breaks its wire contract: {mismatch}. The runtime stamps \
                 these variants as raw wire bytes, so the declaration must keep the order \
                 cataloged in design/ABI.md (expected {expected:?})",
            ));
        }
    });
    match violation {
        Some(message) => Err(LlvmError::Codegen(message)),
        None => Ok(()),
    }
}

/// Verify every registered wire-copied struct declares the field
/// types in [`WIRE_STRUCT_FIELDS`]. Runs once per compile, after
/// struct registration. Skips structs the program never instantiated.
pub(crate) fn assert_wire_struct_layout(ctx: &EmitContext<'_>) -> Result<(), LlvmError> {
    let mut violation = None;
    ctx.layouts.for_each_struct(|symbol, fields| {
        if violation.is_some() {
            return;
        }
        let mangled = symbol.mangled();
        let Some((_, expected)) = WIRE_STRUCT_FIELDS.iter().find(|(name, _)| *name == mangled)
        else {
            return;
        };
        if let Some(mismatch) = wire_layout_mismatch(fields, expected) {
            violation = Some(format!(
                "struct `{mangled}` breaks its wire contract: {mismatch}. The runtime copies \
                 it as raw bytes, so the declaration must keep the layout cataloged in \
                 design/ABI.md (expected {expected:?})",
            ));
        }
    });
    match violation {
        Some(message) => Err(LlvmError::Codegen(message)),
        None => Ok(()),
    }
}

/// Description of the first field type divergence from `expected`, or
/// `None` when the declaration matches the wire layout exactly.
fn wire_layout_mismatch(fields: &[IRType], expected: &[IRType]) -> Option<String> {
    if fields.len() != expected.len() {
        return Some(format!(
            "{} field(s) declared, wire catalog has {}",
            fields.len(),
            expected.len(),
        ));
    }
    for (index, (field, expected_type)) in fields.iter().zip(expected).enumerate() {
        if field != expected_type {
            return Some(format!(
                "field at position {index} is `{field:?}`, wire layout has `{expected_type:?}`",
            ));
        }
    }
    None
}

/// Description of the first tag/name divergence from `expected`, or
/// `None` when the declaration matches the wire order exactly.
fn wire_order_mismatch(variants: &[IREnumVariant], expected: &[&str]) -> Option<String> {
    if variants.len() != expected.len() {
        return Some(format!(
            "{} variant(s) declared, wire catalog has {}",
            variants.len(),
            expected.len(),
        ));
    }
    for (index, (variant, expected_name)) in variants.iter().zip(expected).enumerate() {
        if variant.name != *expected_name || usize::from(variant.tag.0) != index {
            return Some(format!(
                "variant at position {index} is `{}` (tag {}), wire byte {index} means \
                 `{expected_name}`",
                variant.name, variant.tag,
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use koja_ir::{IREnumVariant, IRType, IRVariantPayload, IRVariantTag};

    use super::{wire_layout_mismatch, wire_order_mismatch};

    fn variant(name: &str, tag: u8) -> IREnumVariant {
        IREnumVariant {
            name: name.to_string(),
            payload: IRVariantPayload::Unit,
            tag: IRVariantTag(tag),
        }
    }

    #[test]
    fn matching_wire_order_passes() {
        let variants = [variant("Read", 0), variant("Write", 1), variant("Error", 2)];
        assert_eq!(
            wire_order_mismatch(&variants, &["Read", "Write", "Error"]),
            None,
        );
    }

    #[test]
    fn reordered_variants_are_reported() {
        let variants = [variant("Write", 0), variant("Read", 1), variant("Error", 2)];
        let mismatch = wire_order_mismatch(&variants, &["Read", "Write", "Error"]);
        assert!(mismatch.unwrap().contains("position 0"));
    }

    #[test]
    fn added_variant_is_reported() {
        let variants = [
            variant("Read", 0),
            variant("Write", 1),
            variant("Error", 2),
            variant("Hangup", 3),
        ];
        let mismatch = wire_order_mismatch(&variants, &["Read", "Write", "Error"]);
        assert!(mismatch.unwrap().contains("wire catalog has 3"));
    }

    const FOUR_WORDS: [IRType; 4] = [IRType::Int64, IRType::Int64, IRType::Int64, IRType::Int64];

    #[test]
    fn matching_wire_layout_passes() {
        assert_eq!(wire_layout_mismatch(&FOUR_WORDS, &FOUR_WORDS), None);
    }

    #[test]
    fn narrowed_field_is_reported() {
        let fields = [IRType::Int64, IRType::Int32, IRType::Int64, IRType::Int64];
        let mismatch = wire_layout_mismatch(&fields, &FOUR_WORDS);
        assert!(mismatch.unwrap().contains("position 1"));
    }

    #[test]
    fn added_field_is_reported() {
        let fields = [
            IRType::Int64,
            IRType::Int64,
            IRType::Int64,
            IRType::Int64,
            IRType::Int64,
        ];
        let mismatch = wire_layout_mismatch(&fields, &FOUR_WORDS);
        assert!(mismatch.unwrap().contains("wire catalog has 4"));
    }
}
