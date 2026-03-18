//! Codegen for `Process<M>` intrinsic methods.
//!
//! Process is represented as a bare i64 (the pid). The only method
//! in v1 is `send`, which emits a call to `expo_rt_send`.

use expo_typecheck::types::{Primitive, Type};
use inkwell::AddressSpace;
use inkwell::types::BasicType;

use crate::compiler::Compiler;
use crate::types::to_llvm_type;

pub fn monomorphize_process_struct<'ctx>(
    c: &mut Compiler<'ctx>,
    mangled: &str,
) -> Result<(), String> {
    let i64_type = c.context.i64_type();
    let st = c.context.opaque_struct_type(mangled);
    st.set_body(&[i64_type.into()], false);
    c.struct_types.insert(mangled.to_string(), st);
    c.mono_struct_info.insert(
        mangled.to_string(),
        vec![("pid".to_string(), Type::Primitive(Primitive::I64))],
    );
    Ok(())
}

pub fn emit_process_method<'ctx>(
    c: &mut Compiler<'ctx>,
    mangled_type: &str,
    mangled_fn: &str,
    method_name: &str,
    type_args: &[Type],
) -> Result<(), String> {
    match method_name {
        "send" => {
            let process_struct = *c
                .struct_types
                .get(mangled_type)
                .ok_or_else(|| format!("no LLVM type for `{mangled_type}`"))?;

            let msg_type = type_args
                .first()
                .ok_or("Process.send requires a type argument")?;
            let is_string = matches!(msg_type, Type::Primitive(Primitive::String));

            let msg_llvm = if is_string {
                c.context.ptr_type(AddressSpace::default()).into()
            } else {
                to_llvm_type(msg_type, c.context, &c.struct_types)
                    .ok_or_else(|| format!("no LLVM type for message `{msg_type:?}`"))?
            };

            let fn_type = c
                .context
                .void_type()
                .fn_type(&[process_struct.into(), msg_llvm.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry = c.context.append_basic_block(fn_val, "entry");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let pid = c
                .builder
                .build_extract_value(self_val, 0, "pid")
                .unwrap()
                .into_int_value();

            let send_fn = *c
                .functions
                .get("expo_rt_send")
                .ok_or("expo_rt_send not declared")?;

            let i64_ty = c.context.i64_type();

            if is_string {
                let msg_ptr = fn_val.get_nth_param(1).unwrap().into_pointer_value();
                let strlen_fn = *c.functions.get("strlen").ok_or("strlen not declared")?;
                let str_len = c
                    .builder
                    .build_call(strlen_fn, &[msg_ptr.into()], "str_len")
                    .unwrap()
                    .try_as_basic_value()
                    .left()
                    .ok_or("strlen did not return a value")?
                    .into_int_value();
                let msg_len = c
                    .builder
                    .build_int_add(str_len, i64_ty.const_int(1, false), "msg_len")
                    .unwrap();
                c.builder
                    .build_call(send_fn, &[pid.into(), msg_ptr.into(), msg_len.into()], "")
                    .unwrap();
            } else {
                let msg_val = fn_val.get_nth_param(1).unwrap();
                let ptr_ty = c.context.ptr_type(AddressSpace::default());
                let alloca = c.builder.build_alloca(msg_llvm, "msg_buf").unwrap();
                c.builder.build_store(alloca, msg_val).unwrap();
                let msg_ptr = c
                    .builder
                    .build_pointer_cast(alloca, ptr_ty, "msg_ptr")
                    .unwrap();
                let msg_len = msg_llvm
                    .size_of()
                    .ok_or("cannot compute message byte size")?;
                c.builder
                    .build_call(send_fn, &[pid.into(), msg_ptr.into(), msg_len.into()], "")
                    .unwrap();
            }

            c.builder.build_return(None).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }

            Ok(())
        }
        _ => Err(format!("unknown intrinsic Process method `{method_name}`")),
    }
}
