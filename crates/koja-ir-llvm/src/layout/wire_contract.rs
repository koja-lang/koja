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
//! The check also covers the two payloads the runtime builds field by
//! field, `IO.Ready` and `Process.ExitSignal`. The runtime writes each
//! field at a fixed byte offset, and the emitted receive code reads
//! the payload as the LLVM type of the declaration. The check measures
//! that type and compares each offset and size with the bytes the
//! runtime writes, so a changed field fails the build.
//!
//! The tables mirror ABI.md by spec, not via a shared runtime type,
//! the same policy as `ReceiveTag::wire_byte` (see
//! `koja-runtime-core`'s `wire` module doc). The Rust side of each
//! contract is pinned by unit tests in `koja-runtime-core`, and a
//! unit test here pins the payload offsets against its `wire` module.

use inkwell::types::StructType;
use koja_ir::{IREnumVariant, IRType};

use crate::ctx::EmitContext;
use crate::layout::enums::COMPLETE_PAYLOAD_INDEX;

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

/// `Option<ReplyTo<R>>` wire tag words the envelope packers stamp
/// into send payloads. The receive side loads them as the enum tag,
/// so they must equal the `Option` layout tags, which
/// [`assert_wire_enum_order`] pins to [`OPTION_WIRE_ORDER`].
pub(crate) const OPTION_SOME_TAG: u64 = 0;
pub(crate) const OPTION_NONE_TAG: u64 = 1;

/// Mangled-name prefix every monomorphized `Option` symbol carries.
const OPTION_SYMBOL_PREFIX: &str = "Global.Option_$";

/// Field types of the structs the runtime copies as raw bytes, per
/// ABI.md's context layout. Names and meaning are pinned by the stdlib
/// tests, this table pins the byte layout.
const WIRE_STRUCT_FIELDS: &[(&str, &[IRType])] = &[(
    "Global.Process.Context",
    &[IRType::Int64, IRType::Int64, IRType::Int64, IRType::Int64],
)];

const CRASH_INFO_SYMBOL: &str = "Global.Process.CrashInfo";
const EXIT_REASON_SYMBOL: &str = "Global.Process.ExitReason";
const EXIT_SIGNAL_SYMBOL: &str = "Global.Process.ExitSignal";
const IO_READY_SYMBOL: &str = "Global.IO.Ready";

/// Bytes of an `IO.Ready` payload, and the offset of the descriptor in
/// it. The variant byte is at offset 0. These mirror the `IO_READY_*`
/// constants of the runtime with the tag header taken off.
const IO_READY_SIZE: u64 = 8;
const IO_READY_DESCRIPTOR_OFFSET: u64 = 4;

/// Bytes of a `Process.ExitSignal` payload, and the offsets of the
/// fields the runtime writes in it. These mirror the `EXIT_SIGNAL_*`
/// constants of the runtime with the tag header taken off.
const EXIT_SIGNAL_SIZE: u64 = 32;
const EXIT_SIGNAL_PID_OFFSET: u64 = 0;
const EXIT_SIGNAL_REASON_OFFSET: u64 = 8;
const EXIT_SIGNAL_MESSAGE_OFFSET: u64 = 16;
const EXIT_SIGNAL_BACKTRACE_OFFSET: u64 = 24;

/// One offset or size of a wire payload. `actual` is what the LLVM
/// layout of the declaration gives, or `None` when the declaration
/// has no such part. `expected` is what the runtime writes.
struct Measure {
    what: String,
    actual: Option<u64>,
    expected: u64,
}

impl Measure {
    fn new(what: impl Into<String>, actual: Option<u64>, expected: u64) -> Self {
        Self {
            what: what.into(),
            actual,
            expected,
        }
    }
}

/// Verify every registered wire-coupled enum declares its variants in
/// the ABI.md wire order. Runs once per compile, after enum
/// registration. Skips enums the program never instantiated. Absent
/// from the binary means no wire coupling to protect.
pub(crate) fn assert_wire_enum_order(ctx: &EmitContext<'_>) {
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
    if let Some(message) = violation {
        panic!("{message}");
    }
}

/// Verify every registered wire-copied struct declares the field
/// types in [`WIRE_STRUCT_FIELDS`]. Runs once per compile, after
/// struct registration. Skips structs the program never instantiated.
pub(crate) fn assert_wire_struct_layout(ctx: &EmitContext<'_>) {
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
    if let Some(message) = violation {
        panic!("{message}");
    }
}

/// Verify the LLVM layout of `IO.Ready` and `Process.ExitSignal`
/// against the bytes the runtime writes into their envelopes. Runs
/// once per compile, after every struct and enum body is set. Skips a
/// type the program never instantiated.
pub(crate) fn assert_wire_payload_layout(ctx: &EmitContext<'_>) {
    let payloads = [
        (IO_READY_SYMBOL, io_ready_measures(ctx)),
        (EXIT_SIGNAL_SYMBOL, exit_signal_measures(ctx)),
    ];
    for (name, measures) in payloads {
        let Some(measures) = measures else {
            continue;
        };
        if let Some(mismatch) = wire_payload_mismatch(&measures) {
            panic!(
                "`{name}` breaks its wire contract: {mismatch}. The runtime writes this \
                 payload field by field, so the declaration must keep the layout cataloged \
                 in design/ABI.md",
            );
        }
    }
}

/// The layout of `IO.Ready`, or `None` when the program has no such
/// enum. Every variant must carry the descriptor at the same offset,
/// because the runtime writes it without regard to the variant.
fn io_ready_measures(ctx: &EmitContext<'_>) -> Option<Vec<Measure>> {
    let variants = registered_enum(ctx, IO_READY_SYMBOL)?;
    let target = &ctx.layouts.target_data;
    let descriptor_size = IO_READY_SIZE - IO_READY_DESCRIPTOR_OFFSET;
    let outer = ctx.context.get_struct_type(IO_READY_SYMBOL);
    let mut measures = vec![Measure::new(
        "the size of the enum",
        outer.map(|outer| target.get_abi_size(&outer)),
        IO_READY_SIZE,
    )];
    for variant in &variants {
        let name = &variant.name;
        let (complete, payload) = ctx.layouts.enum_variant_types(IO_READY_SYMBOL, variant.tag);
        measures.push(Measure::new(
            format!("the offset of the descriptor in `{name}`"),
            payload.and_then(|_| target.offset_of_element(&complete, COMPLETE_PAYLOAD_INDEX)),
            IO_READY_DESCRIPTOR_OFFSET,
        ));
        measures.push(Measure::new(
            format!("the size of the descriptor in `{name}`"),
            payload.map(|payload| target.get_abi_size(&payload)),
            descriptor_size,
        ));
    }
    Some(measures)
}

/// The layout of `Process.ExitSignal`, or `None` when the program has
/// no such struct.
fn exit_signal_measures(ctx: &EmitContext<'_>) -> Option<Vec<Measure>> {
    let signal = registered_struct(ctx, EXIT_SIGNAL_SYMBOL)?;
    let target = &ctx.layouts.target_data;
    let reason = target.offset_of_element(&signal, 1);
    let [message, backtrace] = crash_info_offsets(ctx);
    Some(vec![
        Measure::new(
            "the size of the struct",
            Some(target.get_abi_size(&signal)),
            EXIT_SIGNAL_SIZE,
        ),
        Measure::new(
            "the offset of `pid`",
            target.offset_of_element(&signal, 0),
            EXIT_SIGNAL_PID_OFFSET,
        ),
        Measure::new("the offset of `reason`", reason, EXIT_SIGNAL_REASON_OFFSET),
        Measure::new(
            "the offset of `CrashInfo.message`",
            reason.zip(message).map(|(reason, field)| reason + field),
            EXIT_SIGNAL_MESSAGE_OFFSET,
        ),
        Measure::new(
            "the offset of `CrashInfo.backtrace`",
            reason.zip(backtrace).map(|(reason, field)| reason + field),
            EXIT_SIGNAL_BACKTRACE_OFFSET,
        ),
    ])
}

/// Offsets of the two `CrashInfo` fields from the start of a
/// `Process.ExitReason`, through the payload of its `Crashed` variant.
/// `None` for a field the declarations do not have.
fn crash_info_offsets(ctx: &EmitContext<'_>) -> [Option<u64>; 2] {
    let target = &ctx.layouts.target_data;
    let info_offset = || {
        let variants = registered_enum(ctx, EXIT_REASON_SYMBOL)?;
        let crashed = variants.iter().find(|variant| variant.name == "Crashed")?;
        let (complete, payload) = ctx
            .layouts
            .enum_variant_types(EXIT_REASON_SYMBOL, crashed.tag);
        let in_variant = target.offset_of_element(&complete, COMPLETE_PAYLOAD_INDEX)?;
        let in_payload = target.offset_of_element(&payload?, 0)?;
        Some(in_variant + in_payload)
    };
    let field_offset = |index: u32| {
        let info = registered_struct(ctx, CRASH_INFO_SYMBOL)?;
        Some(info_offset()? + target.offset_of_element(&info, index)?)
    };
    [field_offset(0), field_offset(1)]
}

/// The variants of the enum at `mangled`, or `None` when the program
/// never instantiated it.
fn registered_enum(ctx: &EmitContext<'_>, mangled: &str) -> Option<Vec<IREnumVariant>> {
    let mut found = None;
    ctx.layouts.for_each_enum(|symbol, variants| {
        if symbol.mangled() == mangled {
            found = Some(variants.to_vec());
        }
    });
    found
}

/// The LLVM type of the struct at `mangled`, or `None` when the
/// program never instantiated it.
fn registered_struct<'ctx>(ctx: &EmitContext<'ctx>, mangled: &str) -> Option<StructType<'ctx>> {
    let mut registered = false;
    ctx.layouts.for_each_struct(|symbol, _| {
        registered |= symbol.mangled() == mangled;
    });
    registered.then(|| ctx.layouts.struct_type(mangled))
}

/// Description of the first measure that differs from what the runtime
/// writes, or `None` when the layout matches.
fn wire_payload_mismatch(measures: &[Measure]) -> Option<String> {
    let measure = measures
        .iter()
        .find(|measure| measure.actual != Some(measure.expected))?;
    let actual = match measure.actual {
        Some(actual) => actual.to_string(),
        None => "missing".to_string(),
    };
    Some(format!(
        "{} is {actual}, the runtime expects {}",
        measure.what, measure.expected,
    ))
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
    use koja_runtime_core::wire;

    use super::{
        EXIT_SIGNAL_BACKTRACE_OFFSET, EXIT_SIGNAL_MESSAGE_OFFSET, EXIT_SIGNAL_PID_OFFSET,
        EXIT_SIGNAL_REASON_OFFSET, EXIT_SIGNAL_SIZE, IO_READY_DESCRIPTOR_OFFSET, IO_READY_SIZE,
        Measure, wire_layout_mismatch, wire_order_mismatch, wire_payload_mismatch,
    };

    /// A payload offset as the runtime numbers it, from the start of
    /// the envelope.
    fn in_envelope(payload_offset: u64) -> usize {
        wire::TAG_HEADER_SIZE + payload_offset as usize
    }

    /// The payload offsets mirror the runtime's envelope constants by
    /// spec. This dev-dependency-only check turns drift into a test
    /// failure without coupling the compiled crate to the runtime.
    #[test]
    fn payload_offsets_match_the_runtime() {
        assert_eq!(in_envelope(IO_READY_SIZE), wire::IO_READY_BUF_SIZE);
        assert_eq!(in_envelope(0), wire::IO_READY_VARIANT_OFFSET);
        assert_eq!(
            in_envelope(IO_READY_DESCRIPTOR_OFFSET),
            wire::IO_READY_FD_OFFSET,
        );
        assert_eq!(in_envelope(EXIT_SIGNAL_SIZE), wire::EXIT_SIGNAL_BUF_SIZE);
        assert_eq!(
            in_envelope(EXIT_SIGNAL_PID_OFFSET),
            wire::EXIT_SIGNAL_PID_OFFSET,
        );
        assert_eq!(
            in_envelope(EXIT_SIGNAL_REASON_OFFSET),
            wire::EXIT_SIGNAL_REASON_OFFSET,
        );
        assert_eq!(
            in_envelope(EXIT_SIGNAL_MESSAGE_OFFSET),
            wire::EXIT_SIGNAL_MESSAGE_OFFSET,
        );
        assert_eq!(
            in_envelope(EXIT_SIGNAL_BACKTRACE_OFFSET),
            wire::EXIT_SIGNAL_BACKTRACE_OFFSET,
        );
    }

    #[test]
    fn matching_payload_layout_passes() {
        let measures = [
            Measure::new("the size of the enum", Some(8), 8),
            Measure::new("the offset of the descriptor in `Read`", Some(4), 4),
        ];
        assert_eq!(wire_payload_mismatch(&measures), None);
    }

    #[test]
    fn moved_field_is_reported() {
        let measures = [
            Measure::new("the size of the enum", Some(16), 16),
            Measure::new("the offset of the descriptor in `Read`", Some(8), 4),
        ];
        let mismatch = wire_payload_mismatch(&measures).unwrap();
        assert_eq!(
            mismatch,
            "the offset of the descriptor in `Read` is 8, the runtime expects 4",
        );
    }

    #[test]
    fn missing_field_is_reported() {
        let measures = [Measure::new("the offset of `reason`", None, 8)];
        let mismatch = wire_payload_mismatch(&measures).unwrap();
        assert!(mismatch.contains("is missing"));
    }

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
