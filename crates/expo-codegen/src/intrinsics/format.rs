use inkwell::values::FunctionValue;

use crate::compiler::Compiler;
use crate::debug::snprintf_to_expo_string;
use crate::util::bool_to_string_ptr;
use expo_ir::identity::FunctionIdentifier;

pub fn emit_debug_format_intrinsic<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    type_name: &str,
) -> Result<(), String> {
    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    let self_val = fn_val.get_nth_param(0).unwrap();

    match type_name {
        "Bool" => {
            let str_ptr = bool_to_string_ptr(c, self_val.into_int_value());
            c.builder.build_return(Some(&str_ptr)).unwrap();
        }
        "Int" | "Int8" | "Int16" | "Int32" | "UInt8" | "UInt16" | "UInt32" | "UInt64" => {
            let fmt_spec = match type_name {
                "Int" => "%lld",
                "UInt64" => "%llu",
                "Int32" => "%d",
                "UInt32" => "%u",
                "Int16" => "%hd",
                "UInt16" => "%hu",
                "Int8" => "%hhd",
                "UInt8" => "%hhu",
                _ => "%lld",
            };
            let result = snprintf_to_expo_string(c, fmt_spec, &[self_val.into()], "dbg");
            c.builder.build_return(Some(&result)).unwrap();
        }
        "Float" | "Float32" => {
            // Route through `expo_format_{f32,f64}` so the rendered
            // bytes match alpha exactly (shortest round-trip via
            // Rust's `{:?}`), instead of `snprintf("%f")`'s legacy
            // 6-digit fixed precision. One source of truth for both
            // backends — same runtime helpers, same output.
            let symbol = if type_name == "Float32" {
                "expo_format_f32"
            } else {
                "expo_format_f64"
            };
            let rt_fn = *c
                .functions
                .get(&FunctionIdentifier::new(symbol))
                .ok_or_else(|| format!("{symbol} not declared"))?;
            let result = c.call(rt_fn, &[self_val.into()], "float_fmt").unwrap();
            c.builder.build_return(Some(&result)).unwrap();
        }
        "Binary" | "Bits" => {
            let i64_ty = c.context.i64_type();
            let is_bits = i64_ty.const_int(if type_name == "Bits" { 1 } else { 0 }, false);
            let rt_fn = *c
                .functions
                .get(&FunctionIdentifier::new("expo_format_binary"))
                .ok_or("expo_format_binary not declared")?;
            let result = c
                .call(rt_fn, &[self_val.into(), is_bits.into()], "bin_fmt")
                .unwrap();
            c.builder.build_return(Some(&result)).unwrap();
        }
        _ => return Err(format!("unknown debug format intrinsic type: {type_name}")),
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}
