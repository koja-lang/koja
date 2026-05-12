//! Runtime-symbol declarations shared between [`crate::main_wrapper`]
//! (which calls them from the auto-print scaffolding) and
//! [`crate::intrinsics`] (which calls them from compiler-synthesized
//! `@intrinsic` bodies).
//!
//! Each runtime helper lives in `expo-runtime/src/alpha.rs`; this
//! module owns the LLVM-side declarations so the two callers stamp
//! exactly one `module.get_function` lookup per symbol.

use inkwell::AddressSpace;
use inkwell::module::Linkage;
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::FunctionValue;

use crate::ctx::EmitContext;

pub(crate) const CONCAT_BITS_SYMBOL: &str = "__expo_alpha_concat_bits";
pub(crate) const FORMAT_BOOL_SYMBOL: &str = "expo_format_bool";
pub(crate) const FORMAT_F32_SYMBOL: &str = "expo_format_f32";
pub(crate) const FORMAT_F64_SYMBOL: &str = "expo_format_f64";
pub(crate) const FORMAT_I64_SYMBOL: &str = "expo_format_i64";
pub(crate) const FORMAT_U64_SYMBOL: &str = "expo_format_u64";
pub(crate) const FREE_SYMBOL: &str = "free";
pub(crate) const MALLOC_SYMBOL: &str = "malloc";
pub(crate) const MEMSET_SYMBOL: &str = "memset";
pub(crate) const REALLOC_SYMBOL: &str = "realloc";
pub(crate) const PACK_BITS_SYMBOL: &str = "__expo_alpha_pack_bits";
pub(crate) const PANIC_SYMBOL: &str = "__expo_alpha_panic";
pub(crate) const PRINT_BINARY_SYMBOL: &str = "__expo_alpha_print_binary";
pub(crate) const PRINT_BITS_SYMBOL: &str = "__expo_alpha_print_bits";
pub(crate) const PRINT_BOOL_SYMBOL: &str = "__expo_alpha_print_bool";
pub(crate) const PRINT_F32_SYMBOL: &str = "__expo_alpha_print_f32";
pub(crate) const PRINT_F64_SYMBOL: &str = "__expo_alpha_print_f64";
pub(crate) const PRINT_INT_SYMBOL: &str = "__expo_alpha_print_i64";
pub(crate) const PRINT_STRING_SYMBOL: &str = "__expo_alpha_print_string";
pub(crate) const STRCMP_SYMBOL: &str = "strcmp";
pub(crate) const STRING_GET_SYMBOL: &str = "expo_string_get";
pub(crate) const STRING_LENGTH_SYMBOL: &str = "expo_string_length";
pub(crate) const STRING_SLICE_SYMBOL: &str = "expo_string_slice";

/// Get the existing declaration for `symbol` or stamp a fresh
/// `void(arg_type)` external one. Idempotent so callers can declare
/// the same printer from multiple emit sites without duplicating.
pub(crate) fn declare_runtime_printer<'ctx>(
    ctx: &EmitContext<'ctx>,
    symbol: &str,
    argument_type: BasicMetadataTypeEnum<'ctx>,
) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(symbol) {
        return existing;
    }
    let signature = ctx.context.void_type().fn_type(&[argument_type], false);
    ctx.module
        .add_function(symbol, signature, Some(Linkage::External))
}

/// Declare (or look up) one of the `expo_format_*` runtime helpers
/// used by `Debug.format` primitive intrinsics. Signature:
/// `i8* expo_format_<ty>(<argument_type> value)`. Each helper
/// formats `value` into a freshly-allocated length-prefixed Expo
/// string and returns the payload pointer (8 bytes past the
/// `i64 bit_length` header). Single source of truth — the
/// auto-print wrapper calls these too.
pub(crate) fn declare_runtime_format<'ctx>(
    ctx: &EmitContext<'ctx>,
    symbol: &str,
    argument_type: BasicMetadataTypeEnum<'ctx>,
) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(symbol) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ptr_ty.fn_type(&[argument_type], false);
    ctx.module
        .add_function(symbol, signature, Some(Linkage::External))
}

/// Declare (or look up) the libc `free` extern. The drop emitter
/// calls this once per heap-typed slot at function exit. Signature
/// is `void(i8*)`; alpha's heap-block pointers are computed by
/// adjusting the SSA payload pointer (`payload - 8`) before the
/// call so `free` sees the allocator's block base.
pub(crate) fn declare_free_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(FREE_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ctx.context.void_type().fn_type(&[ptr_ty.into()], false);
    ctx.module
        .add_function(FREE_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the libc `malloc` extern. The concat /
/// binary-construct emitters call this for the heap block base.
/// Signature: `i8* malloc(i64)` (alpha targets 64-bit hosts; the
/// argument type matches `size_t` on those targets).
pub(crate) fn declare_malloc_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(MALLOC_SYMBOL) {
        return existing;
    }
    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ptr_ty.fn_type(&[i64_ty.into()], false);
    ctx.module
        .add_function(MALLOC_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the libc `memset` extern. The hashtable
/// `new` emitter calls this to zero-clear the freshly-malloc'd
/// `states` buffer (so every slot starts as `EMPTY`). Signature:
/// `i8* memset(i8* dst, i32 value, i64 n)`.
pub(crate) fn declare_memset_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(MEMSET_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let i32_ty = ctx.context.i32_type();
    let signature = ptr_ty.fn_type(&[ptr_ty.into(), i32_ty.into(), i64_ty.into()], false);
    ctx.module
        .add_function(MEMSET_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the libc `realloc` extern. The list
/// `append` / `concat` emitters call this when the buffer needs to
/// grow. Signature: `i8* realloc(i8* ptr, i64 new_size)`.
pub(crate) fn declare_realloc_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(REALLOC_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false);
    ctx.module
        .add_function(REALLOC_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the `__expo_alpha_concat_bits` runtime
/// helper. Signature: `i8* __expo_alpha_concat_bits(i8* lhs_payload,
/// i8* rhs_payload)`. Reads bit-lengths from each operand's `-8`
/// header, allocates a new `[i64 bit_length][ceil((L+R)/8) bytes]`
/// block, and bit-shifts rhs to land at the lhs trailing partial
/// byte. Sub-byte alignment is far cleaner in Rust than LLVM IR.
pub(crate) fn declare_concat_bits_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(CONCAT_BITS_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
    ctx.module
        .add_function(CONCAT_BITS_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the `__expo_alpha_pack_bits` runtime helper.
/// Signature: `void __expo_alpha_pack_bits(i8* payload, i64 value,
/// i8 width, i64 bit_offset)`. Packs `width` bits of `value` into
/// `payload` MSB-first starting at `bit_offset`. The binary-literal
/// emitter calls this for sub-byte segment widths.
pub(crate) fn declare_pack_bits_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(PACK_BITS_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let signature = ctx.context.void_type().fn_type(
        &[ptr_ty.into(), i64_ty.into(), i8_ty.into(), i64_ty.into()],
        false,
    );
    ctx.module
        .add_function(PACK_BITS_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the libc `strcmp` extern.
/// Signature: `i32 strcmp(i8* lhs, i8* rhs)`. Used by `String.eq`
/// and the generic `==` lowering on `String` operands. Both
/// arguments point to NUL-terminated payload bytes.
pub(crate) fn declare_strcmp_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(STRCMP_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i32_ty = ctx.context.i32_type();
    let signature = i32_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
    ctx.module
        .add_function(STRCMP_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the `expo_string_get` runtime helper.
/// Signature: `i8* expo_string_get(i8* payload, i64 index)`. Returns
/// a freshly-allocated payload for the codepoint at `index`, or
/// `null` when out-of-bounds — the `String.get` emitter branches
/// on the null to mint `None` vs `Some`.
pub(crate) fn declare_string_get_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(STRING_GET_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false);
    ctx.module
        .add_function(STRING_GET_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the `expo_string_length` runtime helper.
/// Signature: `i64 expo_string_length(i8* payload)`. Walks the
/// payload as UTF-8 and returns the Unicode codepoint count.
pub(crate) fn declare_string_length_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(STRING_LENGTH_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = i64_ty.fn_type(&[ptr_ty.into()], false);
    ctx.module
        .add_function(STRING_LENGTH_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the `expo_string_slice` runtime helper.
/// Signature: `i8* expo_string_slice(i8* payload, i64 start, i64 stop)`.
/// Returns a freshly-allocated payload covering the inclusive
/// codepoint range `[start, stop]`; out-of-bounds endpoints clamp to
/// the string boundaries.
pub(crate) fn declare_string_slice_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(STRING_SLICE_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let signature = ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), i64_ty.into()], false);
    ctx.module
        .add_function(STRING_SLICE_SYMBOL, signature, Some(Linkage::External))
}

/// Declare (or look up) the `__expo_alpha_panic` runtime helper.
/// Signature: `void __expo_alpha_panic(i8* message_payload)`. The
/// `Kernel.panic` intrinsic body calls this with the `String`
/// payload pointer (i.e. 8 bytes past the v1 length header) and
/// trails the call with `unreachable`. The runtime side prints
/// `panic: <message>` to stderr and aborts.
pub(crate) fn declare_panic_extern<'ctx>(ctx: &EmitContext<'ctx>) -> FunctionValue<'ctx> {
    if let Some(existing) = ctx.module.get_function(PANIC_SYMBOL) {
        return existing;
    }
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ctx.context.void_type().fn_type(&[ptr_ty.into()], false);
    ctx.module
        .add_function(PANIC_SYMBOL, signature, Some(Linkage::External))
}
