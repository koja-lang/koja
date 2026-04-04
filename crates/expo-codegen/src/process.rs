//! Codegen for `Ref<M, R>` and `ReplyTo<R>` intrinsic methods.
//!
//! Both types are represented as a bare i64 (the pid) at runtime.
//! `Ref<M, R>` supports `cast` and `call`; `ReplyTo<R>` supports `send`.

use expo_typecheck::types::{GenericKind, Primitive, Type, mangle_name, process_envelope_type};
use inkwell::AddressSpace;
use inkwell::types::BasicType;

use crate::compiler::{Compiler, EmitResult};
use crate::types::to_llvm_type;

pub fn monomorphize_ref_struct<'ctx>(c: &mut Compiler<'ctx>, mangled: &str) -> Result<(), String> {
    let i64_type = c.context.i64_type();
    let st = c.context.opaque_struct_type(mangled);
    st.set_body(&[i64_type.into()], false);
    c.types.structs.insert(mangled.to_string(), st);
    c.types.mono_struct_info.insert(
        mangled.to_string(),
        vec![("id".to_string(), Type::Primitive(Primitive::I64))],
    );
    Ok(())
}

pub fn monomorphize_reply_to_struct<'ctx>(
    c: &mut Compiler<'ctx>,
    mangled: &str,
) -> Result<(), String> {
    let i64_type = c.context.i64_type();
    let st = c.context.opaque_struct_type(mangled);
    st.set_body(&[i64_type.into()], false);
    c.types.structs.insert(mangled.to_string(), st);
    c.types.mono_struct_info.insert(
        mangled.to_string(),
        vec![("id".to_string(), Type::Primitive(Primitive::I64))],
    );
    Ok(())
}

fn build_send_body<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: inkwell::values::FunctionValue<'ctx>,
    _msg_type: &Type,
    msg_llvm: inkwell::types::BasicTypeEnum<'ctx>,
    is_string: bool,
) -> Result<(), String> {
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
        let i8_ty = c.context.i8_type();
        let neg8 = i64_ty.const_int((-8i64) as u64, true);
        let hdr_ptr = unsafe {
            c.builder
                .build_gep(i8_ty, msg_ptr, &[neg8], "str_hdr_ptr")
                .unwrap()
        };
        let bit_length = c
            .builder
            .build_load(i64_ty, hdr_ptr, "str_bit_len")
            .unwrap()
            .into_int_value();
        let byte_count = c
            .builder
            .build_right_shift(
                bit_length,
                i64_ty.const_int(3, false),
                false,
                "str_byte_count",
            )
            .unwrap();
        let base_ptr = unsafe {
            c.builder
                .build_gep(i8_ty, msg_ptr, &[neg8], "str_base")
                .unwrap()
        };
        let msg_len = c
            .builder
            .build_int_add(byte_count, i64_ty.const_int(9, false), "msg_len")
            .unwrap();
        c.call_void(send_fn, &[pid.into(), base_ptr.into(), msg_len.into()], "");
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
        c.call_void(send_fn, &[pid.into(), msg_ptr.into(), msg_len.into()], "");
    }

    Ok(())
}

pub fn emit_ref_method<'ctx>(
    c: &mut Compiler<'ctx>,
    mangled_type: &str,
    mangled_fn: &str,
    method_name: &str,
    type_args: &[Type],
) -> Result<EmitResult, String> {
    match method_name {
        "cast" => {
            let ref_struct = *c
                .types
                .structs
                .get(mangled_type)
                .ok_or_else(|| format!("no LLVM type for `{mangled_type}`"))?;

            let msg_type = type_args
                .first()
                .ok_or("Ref.cast requires a type argument")?;
            let reply_type = type_args
                .get(1)
                .ok_or("Ref.cast requires R type argument")?;
            let is_string = matches!(msg_type, Type::Primitive(Primitive::String));

            let msg_llvm = if is_string {
                c.context.ptr_type(AddressSpace::default()).into()
            } else {
                to_llvm_type(msg_type, c.context, &c.types.structs)
                    .ok_or_else(|| format!("no LLVM type for message `{msg_type:?}`"))?
            };

            let envelope_type = process_envelope_type(msg_type, reply_type);
            c.ensure_types_exist(&envelope_type)?;
            let envelope_llvm = to_llvm_type(&envelope_type, c.context, &c.types.structs)
                .ok_or("no LLVM type for Pair envelope")?
                .into_struct_type();

            let fn_type = c
                .context
                .void_type()
                .fn_type(&[ref_struct.into(), msg_llvm.into()], false);
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

            let msg_val = fn_val.get_nth_param(1).unwrap();

            let option_reply_type = Type::GenericInstance {
                base: "Option".to_string(),
                kind: GenericKind::Enum,
                type_args: vec![Type::GenericInstance {
                    base: "ReplyTo".to_string(),
                    kind: GenericKind::Struct,
                    type_args: vec![reply_type.clone()],
                }],
            };
            c.ensure_types_exist(&option_reply_type)?;
            let option_llvm = to_llvm_type(&option_reply_type, c.context, &c.types.structs)
                .ok_or("no LLVM type for Option<ReplyTo<R>>")?
                .into_struct_type();

            let mut option_none = option_llvm.get_undef();
            let tag_none = c.context.i8_type().const_int(1, false);
            option_none = c
                .builder
                .build_insert_value(option_none, tag_none, 0, "none_tag")
                .unwrap()
                .into_struct_value();

            let mut pair_val = envelope_llvm.get_undef();
            pair_val = c
                .builder
                .build_insert_value(pair_val, msg_val, 0, "pair_first")
                .unwrap()
                .into_struct_value();
            pair_val = c
                .builder
                .build_insert_value(pair_val, option_none, 1, "pair_second")
                .unwrap()
                .into_struct_value();

            let send_fn = *c
                .functions
                .get("expo_rt_send")
                .ok_or("expo_rt_send not declared")?;

            let ptr_ty = c.context.ptr_type(AddressSpace::default());
            let alloca = c
                .builder
                .build_alloca(envelope_llvm, "envelope_buf")
                .unwrap();
            c.builder.build_store(alloca, pair_val).unwrap();
            let msg_ptr = c
                .builder
                .build_pointer_cast(alloca, ptr_ty, "envelope_ptr")
                .unwrap();
            let msg_len = envelope_llvm
                .size_of()
                .ok_or("cannot compute envelope byte size")?;
            c.call_void(send_fn, &[pid.into(), msg_ptr.into(), msg_len.into()], "");

            c.builder.build_return(None).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }

            Ok(EmitResult::Emitted)
        }
        "call" => {
            let ref_struct = *c
                .types
                .structs
                .get(mangled_type)
                .ok_or_else(|| format!("no LLVM type for `{mangled_type}`"))?;

            let msg_type = type_args
                .first()
                .ok_or("Ref.call requires M type argument")?;
            let reply_type = type_args
                .get(1)
                .ok_or("Ref.call requires R type argument")?;
            let is_msg_string = matches!(msg_type, Type::Primitive(Primitive::String));
            let is_reply_string = matches!(reply_type, Type::Primitive(Primitive::String));
            let is_msg_unit = matches!(msg_type, Type::Unit);
            let is_reply_unit = matches!(reply_type, Type::Unit);

            let msg_llvm = if is_msg_string {
                c.context.ptr_type(AddressSpace::default()).into()
            } else if is_msg_unit {
                // ZST message; i8 placeholder matches Pair<Unit,_> envelope field layout.
                c.context.i8_type().into()
            } else {
                to_llvm_type(msg_type, c.context, &c.types.structs)
                    .ok_or_else(|| format!("no LLVM type for call message `{msg_type:?}`"))?
            };

            let i64_ty = c.context.i64_type();

            let reply_llvm = if is_reply_string {
                c.context.ptr_type(AddressSpace::default()).into()
            } else if is_reply_unit {
                c.context.i8_type().into()
            } else {
                to_llvm_type(reply_type, c.context, &c.types.structs)
                    .ok_or_else(|| format!("no LLVM type for reply `{reply_type:?}`"))?
            };

            let option_reply_mangled = mangle_name("Option", std::slice::from_ref(reply_type));
            if !c.types.structs.contains_key(&option_reply_mangled) {
                c.monomorphize_enum("Option", std::slice::from_ref(reply_type))?;
            }
            let option_reply_struct = *c
                .types
                .structs
                .get(&option_reply_mangled)
                .ok_or("Option struct not found for call reply")?;

            let envelope_type = process_envelope_type(msg_type, reply_type);
            c.ensure_types_exist(&envelope_type)?;
            let envelope_llvm = to_llvm_type(&envelope_type, c.context, &c.types.structs)
                .ok_or("no LLVM type for Pair envelope")?
                .into_struct_type();

            let reply_to_type = Type::GenericInstance {
                base: "ReplyTo".to_string(),
                kind: GenericKind::Struct,
                type_args: vec![reply_type.clone()],
            };
            c.ensure_types_exist(&reply_to_type)?;
            let reply_to_llvm = to_llvm_type(&reply_to_type, c.context, &c.types.structs)
                .ok_or("no LLVM type for ReplyTo<R>")?
                .into_struct_type();

            let option_from_type = Type::GenericInstance {
                base: "Option".to_string(),
                kind: GenericKind::Enum,
                type_args: vec![reply_to_type],
            };
            c.ensure_types_exist(&option_from_type)?;
            let option_from_llvm = to_llvm_type(&option_from_type, c.context, &c.types.structs)
                .ok_or("no LLVM type for Option<ReplyTo<R>>")?
                .into_struct_type();

            let fn_type = option_reply_struct
                .fn_type(&[ref_struct.into(), msg_llvm.into(), i64_ty.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry = c.context.append_basic_block(fn_val, "entry");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let target_pid = c
                .builder
                .build_extract_value(self_val, 0, "target_pid")
                .unwrap()
                .into_int_value();
            let msg_val = fn_val.get_nth_param(1).unwrap();

            let self_fn = *c
                .functions
                .get("expo_rt_self")
                .ok_or("expo_rt_self not declared")?;
            let caller_pid = c
                .call(self_fn, &[], "caller_pid")
                .ok_or("expo_rt_self did not return a value")?
                .into_int_value();

            let mut reply_to_val = reply_to_llvm.get_undef();
            reply_to_val = c
                .builder
                .build_insert_value(reply_to_val, caller_pid, 0, "reply_to_id")
                .unwrap()
                .into_struct_value();

            let option_some = {
                let alloca = c
                    .builder
                    .build_alloca(option_from_llvm, "option_some_buf")
                    .unwrap();
                let tag_ptr = c
                    .builder
                    .build_struct_gep(option_from_llvm, alloca, 0, "tag_ptr")
                    .unwrap();
                let tag_some = c.context.i8_type().const_int(0, false);
                c.builder.build_store(tag_ptr, tag_some).unwrap();
                let payload_ptr = c
                    .builder
                    .build_struct_gep(option_from_llvm, alloca, 1, "payload_ptr")
                    .unwrap();
                let typed_ptr = c
                    .builder
                    .build_pointer_cast(
                        payload_ptr,
                        c.context.ptr_type(AddressSpace::default()),
                        "typed_payload_ptr",
                    )
                    .unwrap();
                c.builder.build_store(typed_ptr, reply_to_val).unwrap();
                c.builder
                    .build_load(option_from_llvm, alloca, "option_some")
                    .unwrap()
                    .into_struct_value()
            };

            let mut pair_val = envelope_llvm.get_undef();
            pair_val = c
                .builder
                .build_insert_value(pair_val, msg_val, 0, "pair_first")
                .unwrap()
                .into_struct_value();
            pair_val = c
                .builder
                .build_insert_value(pair_val, option_some, 1, "pair_second")
                .unwrap()
                .into_struct_value();

            let send_fn = *c
                .functions
                .get("expo_rt_send")
                .ok_or("expo_rt_send not declared")?;

            let ptr_ty = c.context.ptr_type(AddressSpace::default());
            let alloca = c
                .builder
                .build_alloca(envelope_llvm, "envelope_buf")
                .unwrap();
            c.builder.build_store(alloca, pair_val).unwrap();
            let msg_ptr = c
                .builder
                .build_pointer_cast(alloca, ptr_ty, "envelope_ptr")
                .unwrap();
            let msg_len = envelope_llvm
                .size_of()
                .ok_or("cannot compute envelope byte size")?;
            c.call_void(
                send_fn,
                &[target_pid.into(), msg_ptr.into(), msg_len.into()],
                "",
            );

            let timeout_val = fn_val.get_nth_param(2).unwrap().into_int_value();

            let receive_timeout_fn = *c
                .functions
                .get("expo_rt_receive_timeout")
                .ok_or("expo_rt_receive_timeout not declared")?;

            let raw_ptr = c
                .call(receive_timeout_fn, &[timeout_val.into()], "receive_reply")
                .ok_or("expo_rt_receive_timeout did not return a value")?
                .into_pointer_value();

            let null_ptr = ptr_ty.const_null();
            let is_null = c
                .builder
                .build_int_compare(inkwell::IntPredicate::EQ, raw_ptr, null_ptr, "is_timeout")
                .unwrap();

            let then_bb = c.context.append_basic_block(fn_val, "timeout");
            let else_bb = c.context.append_basic_block(fn_val, "got_reply");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_null, then_bb, else_bb)
                .unwrap();

            c.builder.position_at_end(then_bb);
            let mut none_val = option_reply_struct.get_undef();
            let tag_none = c.context.i8_type().const_int(1, false);
            none_val = c
                .builder
                .build_insert_value(none_val, tag_none, 0, "none_tag")
                .unwrap()
                .into_struct_value();
            c.builder.build_unconditional_branch(merge_bb).unwrap();

            c.builder.position_at_end(else_bb);
            let i8_ty_reply = c.context.i8_type();
            let reply_payload_ptr = unsafe {
                c.builder
                    .build_in_bounds_gep(
                        i8_ty_reply,
                        raw_ptr,
                        &[c.context.i64_type().const_int(8, false)],
                        "reply_payload",
                    )
                    .unwrap()
            };
            let reply_val = if is_reply_string {
                let str_ptr = unsafe {
                    c.builder
                        .build_in_bounds_gep(
                            i8_ty_reply,
                            raw_ptr,
                            &[c.context.i64_type().const_int(16, false)],
                            "reply_str_ptr",
                        )
                        .unwrap()
                };
                str_ptr.into()
            } else {
                c.builder
                    .build_load(reply_llvm, reply_payload_ptr, "reply_val")
                    .unwrap()
            };
            let some_val = {
                let alloca = c
                    .builder
                    .build_alloca(option_reply_struct, "some_reply_buf")
                    .unwrap();
                let tag_ptr = c
                    .builder
                    .build_struct_gep(option_reply_struct, alloca, 0, "reply_tag_ptr")
                    .unwrap();
                let tag_some_reply = c.context.i8_type().const_int(0, false);
                c.builder.build_store(tag_ptr, tag_some_reply).unwrap();
                let payload_ptr = c
                    .builder
                    .build_struct_gep(option_reply_struct, alloca, 1, "reply_payload_ptr")
                    .unwrap();
                let typed_ptr = c
                    .builder
                    .build_pointer_cast(
                        payload_ptr,
                        c.context.ptr_type(AddressSpace::default()),
                        "reply_typed_ptr",
                    )
                    .unwrap();
                c.builder.build_store(typed_ptr, reply_val).unwrap();
                c.builder
                    .build_load(option_reply_struct, alloca, "some_val")
                    .unwrap()
                    .into_struct_value()
            };
            c.builder.build_unconditional_branch(merge_bb).unwrap();

            c.builder.position_at_end(merge_bb);
            let phi = c
                .builder
                .build_phi(option_reply_struct, "call_result")
                .unwrap();
            phi.add_incoming(&[(&none_val, then_bb), (&some_val, else_bb)]);

            c.builder.build_return(Some(&phi.as_basic_value())).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }

            Ok(EmitResult::Emitted)
        }
        _ => Ok(EmitResult::NotIntrinsic),
    }
}

pub fn emit_reply_to_method<'ctx>(
    c: &mut Compiler<'ctx>,
    mangled_type: &str,
    mangled_fn: &str,
    method_name: &str,
    type_args: &[Type],
) -> Result<EmitResult, String> {
    match method_name {
        "send" => {
            let reply_to_struct = *c
                .types
                .structs
                .get(mangled_type)
                .ok_or_else(|| format!("no LLVM type for `{mangled_type}`"))?;

            let reply_type = type_args
                .first()
                .ok_or("ReplyTo.send requires a type argument")?;
            let is_string = matches!(reply_type, Type::Primitive(Primitive::String));

            let reply_llvm = if is_string {
                c.context.ptr_type(AddressSpace::default()).into()
            } else {
                to_llvm_type(reply_type, c.context, &c.types.structs)
                    .ok_or_else(|| format!("no LLVM type for reply `{reply_type:?}`"))?
            };

            let fn_type = c
                .context
                .void_type()
                .fn_type(&[reply_to_struct.into(), reply_llvm.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry = c.context.append_basic_block(fn_val, "entry");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry);

            build_send_body(c, fn_val, reply_type, reply_llvm, is_string)?;

            c.builder.build_return(None).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }

            Ok(EmitResult::Emitted)
        }
        _ => Ok(EmitResult::NotIntrinsic),
    }
}
