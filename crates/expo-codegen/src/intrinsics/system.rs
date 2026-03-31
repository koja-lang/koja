use expo_typecheck::types::{GenericKind, Primitive, Type};
use inkwell::IntPredicate;
use inkwell::values::FunctionValue;

use crate::compiler::Compiler;

use super::{build_result_err, build_result_ok};

pub fn emit_system_intrinsic<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    mangled: &str,
) -> Result<(), String> {
    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    match mangled {
        "System_get_env" => {
            let option_mangled = "Option_$String$";
            c.ensure_types_exist(&Type::GenericInstance {
                base: "Option".to_string(),
                type_args: vec![Type::Primitive(Primitive::String)],
                kind: GenericKind::Enum,
            })?;
            let option_struct = *c
                .types
                .structs
                .get(option_mangled)
                .ok_or("no LLVM type for Option_$String$")?;

            let key_ptr = fn_val.get_nth_param(0).unwrap();
            let rt_fn = *c
                .functions
                .get("expo_get_env")
                .ok_or("expo_get_env not declared")?;
            let raw_ptr = c
                .call(rt_fn, &[key_ptr.into()], "env_val")
                .unwrap()
                .into_pointer_value();

            let i8_ty = c.context.i8_type();
            let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());
            let is_null = c
                .builder
                .build_int_compare(IntPredicate::EQ, raw_ptr, ptr_ty.const_null(), "is_null")
                .unwrap();

            let some_bb = c.context.append_basic_block(fn_val, "some");
            let none_bb = c.context.append_basic_block(fn_val, "none");
            c.builder
                .build_conditional_branch(is_null, none_bb, some_bb)
                .unwrap();

            c.builder.position_at_end(some_bb);
            let alloca_some = c
                .builder
                .build_alloca(option_struct, "some_alloca")
                .unwrap();
            let tag_ptr = c
                .builder
                .build_struct_gep(option_struct, alloca_some, 0, "tag_ptr")
                .unwrap();
            c.builder
                .build_store(tag_ptr, i8_ty.const_int(0, false))
                .unwrap();
            let payload_ptr = c
                .builder
                .build_struct_gep(option_struct, alloca_some, 1, "payload_ptr")
                .unwrap();
            c.builder.build_store(payload_ptr, raw_ptr).unwrap();
            let result = c
                .builder
                .build_load(option_struct, alloca_some, "some_val")
                .unwrap();
            c.builder.build_return(Some(&result)).unwrap();

            c.builder.position_at_end(none_bb);
            let alloca_none = c
                .builder
                .build_alloca(option_struct, "none_alloca")
                .unwrap();
            let tag_ptr = c
                .builder
                .build_struct_gep(option_struct, alloca_none, 0, "tag_ptr")
                .unwrap();
            c.builder
                .build_store(tag_ptr, i8_ty.const_int(1, false))
                .unwrap();
            let result = c
                .builder
                .build_load(option_struct, alloca_none, "none_val")
                .unwrap();
            c.builder.build_return(Some(&result)).unwrap();
        }
        "System_set_env" => {
            let key_ptr = fn_val.get_nth_param(0).unwrap();
            let val_ptr = fn_val.get_nth_param(1).unwrap();
            let rt_fn = *c
                .functions
                .get("expo_set_env")
                .ok_or("expo_set_env not declared")?;
            c.call(rt_fn, &[key_ptr.into(), val_ptr.into()], "");
            c.builder.build_return(None).unwrap();
        }
        "System_cwd" => {
            let result_type = fn_val
                .get_type()
                .get_return_type()
                .unwrap()
                .into_struct_type();
            let rt_fn = *c.functions.get("expo_cwd").ok_or("expo_cwd not declared")?;
            let raw_ptr = c.call(rt_fn, &[], "cwd_ptr").unwrap().into_pointer_value();

            let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());
            let is_null = c
                .builder
                .build_int_compare(IntPredicate::EQ, raw_ptr, ptr_ty.const_null(), "is_null")
                .unwrap();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_null, err_bb, ok_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);
            let ok_result = build_result_ok(c, raw_ptr.into(), result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let ok_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(err_bb);
            let err_fn = *c
                .functions
                .get("expo_last_error")
                .ok_or("expo_last_error not declared")?;
            let err_msg = c.call(err_fn, &[], "err_msg").unwrap();
            let err_result = build_result_err(c, err_msg, result_type);
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            let err_end = c.builder.get_insert_block().unwrap();

            c.builder.position_at_end(merge_bb);
            let phi = c.builder.build_phi(result_type, "result").unwrap();
            phi.add_incoming(&[(&ok_result, ok_end), (&err_result, err_end)]);
            c.builder.build_return(Some(&phi.as_basic_value())).unwrap();
        }
        "System_hostname" => {
            let rt_fn = *c
                .functions
                .get("expo_hostname")
                .ok_or("expo_hostname not declared")?;
            let ptr = c.call(rt_fn, &[], "hostname_ptr").unwrap();
            c.builder.build_return(Some(&ptr)).unwrap();
        }
        _ => return Err(format!("unknown system intrinsic: {mangled}")),
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}

pub fn emit_time_intrinsic<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    mangled: &str,
) -> Result<(), String> {
    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    match mangled {
        "DateTime_now" => {
            let rt_fn = *c
                .functions
                .get("expo_time_now_millis")
                .ok_or("expo_time_now_millis not declared")?;
            let millis = c.call(rt_fn, &[], "millis").unwrap().into_int_value();

            let dt_struct_ty = *c
                .types
                .structs
                .get("DateTime")
                .ok_or("DateTime struct type not found")?;
            let alloca = c.builder.build_alloca(dt_struct_ty, "dt_tmp").unwrap();
            let field_ptr = c
                .builder
                .build_struct_gep(dt_struct_ty, alloca, 0, "millis_field")
                .unwrap();
            c.builder.build_store(field_ptr, millis).unwrap();
            let dt_val = c
                .builder
                .build_load(dt_struct_ty, alloca, "dt_val")
                .unwrap();
            c.builder.build_return(Some(&dt_val)).unwrap();
        }
        _ => return Err(format!("unknown time intrinsic: {mangled}")),
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}
