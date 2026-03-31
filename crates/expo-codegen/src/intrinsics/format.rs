use inkwell::values::FunctionValue;

use crate::compiler::Compiler;
use crate::debug::snprintf_to_expo_string;
use crate::util::bool_to_string_ptr;

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
                "Int" | "UInt64" => "%lld",
                "Int32" | "UInt32" => "%d",
                "Int16" | "UInt16" => "%hd",
                "Int8" | "UInt8" => "%hhd",
                _ => "%lld",
            };
            let result = snprintf_to_expo_string(c, fmt_spec, &[self_val.into()], "dbg");
            c.builder.build_return(Some(&result)).unwrap();
        }
        "Float" | "Float32" => {
            let f64_ty = c.context.f64_type();
            let val = if type_name == "Float32" {
                let ext = c
                    .builder
                    .build_float_ext(self_val.into_float_value(), f64_ty, "f64_ext")
                    .unwrap();
                ext.into()
            } else {
                self_val
            };
            let result = snprintf_to_expo_string(c, "%f", &[val.into()], "dbg");
            c.builder.build_return(Some(&result)).unwrap();
        }
        "Binary" | "Bits" => {
            let i64_ty = c.context.i64_type();
            let is_bits = i64_ty.const_int(if type_name == "Bits" { 1 } else { 0 }, false);
            let rt_fn = *c
                .functions
                .get("expo_format_binary")
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
