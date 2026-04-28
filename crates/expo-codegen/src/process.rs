//! Codegen for `Ref<M, R>` and `ReplyTo<R>` intrinsic methods.
//!
//! Both types are represented as a bare i64 (the pid) at runtime.
//! `Ref<M, R>` supports `cast` and `call`; `ReplyTo<R>` supports `send`.

use expo_ast::identifier::TypeIdentifier;
use expo_ast::types::{named_generic_std, named_std};
use expo_typecheck::types::{Primitive, Type, mangle_name, process_envelope_type};
use inkwell::AddressSpace;
use inkwell::types::BasicType;

use crate::compiler::{Compiler, EmitResult};
use crate::generics::{ensure_types_exist, monomorphize_enum};
use crate::types::to_llvm_type;
use expo_ir::identity::{FunctionIdentifier, MonomorphizedTypeIdentifier};

pub fn monomorphize_ref_struct<'ctx>(c: &mut Compiler<'ctx>, mangled: &str) -> Result<(), String> {
    let i64_type = c.context.i64_type();
    let st = c.context.opaque_struct_type(mangled);
    st.set_body(&[i64_type.into()], false);
    c.llvm_types
        .register_monomorphized(MonomorphizedTypeIdentifier::new(mangled), st);
    c.layouts.register_struct_layout(
        MonomorphizedTypeIdentifier::new(mangled),
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
    c.llvm_types
        .register_monomorphized(MonomorphizedTypeIdentifier::new(mangled), st);
    c.layouts.register_struct_layout(
        MonomorphizedTypeIdentifier::new(mangled),
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
        .get(&FunctionIdentifier::new("expo_rt_send"))
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

/// Builds a `Result.Err(CallError.Variant)` value as a struct.
/// `callerror_tag` is the CallError variant index (0=Timeout, 1=ProcessDown).
fn build_result_err<'ctx>(
    c: &mut Compiler<'ctx>,
    result_struct: inkwell::types::StructType<'ctx>,
    i8_ty: inkwell::types::IntType<'ctx>,
    callerror_tag: u64,
) -> Result<inkwell::values::StructValue<'ctx>, String> {
    let ptr_ty = c.context.ptr_type(AddressSpace::default());
    let alloca = c.builder.build_alloca(result_struct, "err_buf").unwrap();
    // Result.Err tag = 1
    let tag_ptr = c
        .builder
        .build_struct_gep(result_struct, alloca, 0, "err_tag_ptr")
        .unwrap();
    c.builder
        .build_store(tag_ptr, i8_ty.const_int(1, false))
        .unwrap();
    // CallError is { i8 } -- write the variant tag into the payload.
    let payload_ptr = c
        .builder
        .build_struct_gep(result_struct, alloca, 1, "err_payload_ptr")
        .unwrap();
    let typed_ptr = c
        .builder
        .build_pointer_cast(payload_ptr, ptr_ty, "err_typed_ptr")
        .unwrap();
    c.builder
        .build_store(typed_ptr, i8_ty.const_int(callerror_tag, false))
        .unwrap();
    Ok(c.builder
        .build_load(result_struct, alloca, "err_val")
        .unwrap()
        .into_struct_value())
}

pub fn emit_ref_method<'ctx>(
    c: &mut Compiler<'ctx>,
    mangled_type: &str,
    mangled_fn: &str,
    method_name: &str,
    type_args: &[Type],
) -> Result<EmitResult, String> {
    match method_name {
        "self_ref" => {
            let ref_struct = c
                .llvm_types
                .get_monomorphized(&MonomorphizedTypeIdentifier::new(mangled_type))
                .ok_or_else(|| format!("no LLVM type for `{mangled_type}`"))?;

            let fn_type = ref_struct.fn_type(&[], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.register_intrinsic(
                FunctionIdentifier::new(mangled_fn),
                fn_val,
                "Ref",
                method_name,
            );

            let entry = c.context.append_basic_block(fn_val, "entry");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry);

            let self_fn = *c
                .functions
                .get(&FunctionIdentifier::new("expo_rt_self"))
                .ok_or("expo_rt_self not declared")?;
            let pid = c
                .call(self_fn, &[], "current_pid")
                .ok_or("expo_rt_self did not return a value")?
                .into_int_value();

            let mut ref_val = ref_struct.get_undef();
            ref_val = c
                .builder
                .build_insert_value(ref_val, pid, 0, "ref_id")
                .unwrap()
                .into_struct_value();

            c.builder.build_return(Some(&ref_val)).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }

            Ok(EmitResult::Emitted)
        }
        "cast" => {
            let ref_struct = c
                .llvm_types
                .get_monomorphized(&MonomorphizedTypeIdentifier::new(mangled_type))
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
                to_llvm_type(msg_type, c.context, &c.llvm_types)
                    .ok_or_else(|| format!("no LLVM type for message `{msg_type:?}`"))?
            };

            let envelope_type = process_envelope_type(msg_type, reply_type);
            ensure_types_exist(c, &envelope_type)?;
            let envelope_llvm = to_llvm_type(&envelope_type, c.context, &c.llvm_types)
                .ok_or("no LLVM type for Pair envelope")?
                .into_struct_type();

            let fn_type = c
                .context
                .void_type()
                .fn_type(&[ref_struct.into(), msg_llvm.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.register_intrinsic(
                FunctionIdentifier::new(mangled_fn),
                fn_val,
                "Ref",
                method_name,
            );

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

            let option_reply_type = named_generic_std(
                "Option",
                vec![named_generic_std("ReplyTo", vec![reply_type.clone()])],
            );
            ensure_types_exist(c, &option_reply_type)?;
            let option_llvm = to_llvm_type(&option_reply_type, c.context, &c.llvm_types)
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
                .get(&FunctionIdentifier::new("expo_rt_send"))
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
            let ref_struct = c
                .llvm_types
                .get_monomorphized(&MonomorphizedTypeIdentifier::new(mangled_type))
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
                c.context.i8_type().into()
            } else {
                to_llvm_type(msg_type, c.context, &c.llvm_types)
                    .ok_or_else(|| format!("no LLVM type for call message `{msg_type:?}`"))?
            };

            let i64_ty = c.context.i64_type();
            let i8_ty = c.context.i8_type();
            let ptr_ty = c.context.ptr_type(AddressSpace::default());

            let reply_llvm = if is_reply_string {
                ptr_ty.into()
            } else if is_reply_unit {
                i8_ty.into()
            } else {
                to_llvm_type(reply_type, c.context, &c.llvm_types)
                    .ok_or_else(|| format!("no LLVM type for reply `{reply_type:?}`"))?
            };

            // Monomorphize Result<R, CallError> as return type.
            let callerror_type = named_std("CallError");
            let result_type_args = vec![reply_type.clone(), callerror_type.clone()];
            let result_id = TypeIdentifier::std("Result");
            let result_mangled = mangle_name(&result_id, &result_type_args);
            if !c
                .llvm_types
                .contains_monomorphized(&MonomorphizedTypeIdentifier::new(&result_mangled))
            {
                monomorphize_enum(c, &result_id, &result_type_args)?;
            }
            let result_struct = c
                .llvm_types
                .get_monomorphized(&MonomorphizedTypeIdentifier::new(&result_mangled))
                .ok_or("Result<R, CallError> struct not found")?;

            // Ensure CallError enum exists in LLVM.
            if c.llvm_types
                .get_concrete(&TypeIdentifier::std("CallError"))
                .is_none()
            {
                monomorphize_enum(c, &TypeIdentifier::std("CallError"), &[])?;
            }

            let envelope_type = process_envelope_type(msg_type, reply_type);
            ensure_types_exist(c, &envelope_type)?;
            let envelope_llvm = to_llvm_type(&envelope_type, c.context, &c.llvm_types)
                .ok_or("no LLVM type for Pair envelope")?
                .into_struct_type();

            let reply_to_type = named_generic_std("ReplyTo", vec![reply_type.clone()]);
            ensure_types_exist(c, &reply_to_type)?;
            let reply_to_llvm = to_llvm_type(&reply_to_type, c.context, &c.llvm_types)
                .ok_or("no LLVM type for ReplyTo<R>")?
                .into_struct_type();

            let option_from_type = named_generic_std("Option", vec![reply_to_type]);
            ensure_types_exist(c, &option_from_type)?;
            let option_from_llvm = to_llvm_type(&option_from_type, c.context, &c.llvm_types)
                .ok_or("no LLVM type for Option<ReplyTo<R>>")?
                .into_struct_type();

            let fn_type =
                result_struct.fn_type(&[ref_struct.into(), msg_llvm.into(), i64_ty.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.register_intrinsic(
                FunctionIdentifier::new(mangled_fn),
                fn_val,
                "Ref",
                method_name,
            );

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
                .get(&FunctionIdentifier::new("expo_rt_self"))
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

            // Build Option.Some(reply_to) for the envelope.
            let option_some = {
                let alloca = c
                    .builder
                    .build_alloca(option_from_llvm, "option_some_buf")
                    .unwrap();
                let tag_ptr = c
                    .builder
                    .build_struct_gep(option_from_llvm, alloca, 0, "tag_ptr")
                    .unwrap();
                let tag_some = i8_ty.const_int(0, false);
                c.builder.build_store(tag_ptr, tag_some).unwrap();
                let payload_ptr = c
                    .builder
                    .build_struct_gep(option_from_llvm, alloca, 1, "payload_ptr")
                    .unwrap();
                let typed_ptr = c
                    .builder
                    .build_pointer_cast(payload_ptr, ptr_ty, "typed_payload_ptr")
                    .unwrap();
                c.builder.build_store(typed_ptr, reply_to_val).unwrap();
                c.builder
                    .build_load(option_from_llvm, alloca, "option_some")
                    .unwrap()
                    .into_struct_value()
            };

            // Build the Pair<M, Option<ReplyTo<R>>> envelope.
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
                .get(&FunctionIdentifier::new("expo_rt_send"))
                .ok_or("expo_rt_send not declared")?;

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

            // Wait for reply with timeout.
            let timeout_val = fn_val.get_nth_param(2).unwrap().into_int_value();

            let receive_timeout_fn = *c
                .functions
                .get(&FunctionIdentifier::new("expo_rt_receive_timeout"))
                .ok_or("expo_rt_receive_timeout not declared")?;

            let raw_ptr = c
                .call(receive_timeout_fn, &[timeout_val.into()], "receive_reply")
                .ok_or("expo_rt_receive_timeout did not return a value")?
                .into_pointer_value();

            let null_ptr = ptr_ty.const_null();
            let is_null = c
                .builder
                .build_int_compare(inkwell::IntPredicate::EQ, raw_ptr, null_ptr, "is_null")
                .unwrap();

            // Three-way branch: got_reply / check_alive / timeout / process_down.
            let got_reply_bb = c.context.append_basic_block(fn_val, "got_reply");
            let check_alive_bb = c.context.append_basic_block(fn_val, "check_alive");
            let timeout_bb = c.context.append_basic_block(fn_val, "timeout");
            let process_down_bb = c.context.append_basic_block(fn_val, "process_down");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_null, check_alive_bb, got_reply_bb)
                .unwrap();

            // -- check_alive: distinguish Timeout from ProcessDown --
            c.builder.position_at_end(check_alive_bb);
            let is_alive_fn = *c
                .functions
                .get(&FunctionIdentifier::new("expo_rt_is_process_alive"))
                .ok_or("expo_rt_is_process_alive not declared")?;
            let alive_result = c
                .call(is_alive_fn, &[target_pid.into()], "alive")
                .ok_or("expo_rt_is_process_alive did not return a value")?
                .into_int_value();
            let is_alive = c
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::NE,
                    alive_result,
                    i64_ty.const_int(0, false),
                    "is_alive",
                )
                .unwrap();
            c.builder
                .build_conditional_branch(is_alive, timeout_bb, process_down_bb)
                .unwrap();

            // -- timeout: Result.Err(CallError.Timeout) --
            // CallError.Timeout = tag 0
            c.builder.position_at_end(timeout_bb);
            let timeout_result = build_result_err(c, result_struct, i8_ty, 0)?;
            c.builder.build_unconditional_branch(merge_bb).unwrap();

            // -- process_down: Result.Err(CallError.ProcessDown) --
            // CallError.ProcessDown = tag 1
            c.builder.position_at_end(process_down_bb);
            let down_result = build_result_err(c, result_struct, i8_ty, 1)?;
            c.builder.build_unconditional_branch(merge_bb).unwrap();

            // -- got_reply: Result.Ok(reply) --
            c.builder.position_at_end(got_reply_bb);
            let reply_payload_ptr = unsafe {
                c.builder
                    .build_in_bounds_gep(
                        i8_ty,
                        raw_ptr,
                        &[i64_ty.const_int(8, false)],
                        "reply_payload",
                    )
                    .unwrap()
            };
            let reply_val = if is_reply_string {
                let str_ptr = unsafe {
                    c.builder
                        .build_in_bounds_gep(
                            i8_ty,
                            raw_ptr,
                            &[i64_ty.const_int(16, false)],
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
            // Result.Ok tag = 0
            let ok_result = {
                let alloca = c.builder.build_alloca(result_struct, "ok_buf").unwrap();
                let tag_ptr = c
                    .builder
                    .build_struct_gep(result_struct, alloca, 0, "ok_tag_ptr")
                    .unwrap();
                c.builder
                    .build_store(tag_ptr, i8_ty.const_int(0, false))
                    .unwrap();
                let payload_ptr = c
                    .builder
                    .build_struct_gep(result_struct, alloca, 1, "ok_payload_ptr")
                    .unwrap();
                let typed_ptr = c
                    .builder
                    .build_pointer_cast(payload_ptr, ptr_ty, "ok_typed_ptr")
                    .unwrap();
                c.builder.build_store(typed_ptr, reply_val).unwrap();
                c.builder
                    .build_load(result_struct, alloca, "ok_val")
                    .unwrap()
                    .into_struct_value()
            };
            c.builder.build_unconditional_branch(merge_bb).unwrap();

            // -- merge --
            c.builder.position_at_end(merge_bb);
            let phi = c.builder.build_phi(result_struct, "call_result").unwrap();
            phi.add_incoming(&[
                (&ok_result, got_reply_bb),
                (&timeout_result, timeout_bb),
                (&down_result, process_down_bb),
            ]);

            c.builder.build_return(Some(&phi.as_basic_value())).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }

            Ok(EmitResult::Emitted)
        }
        "signal" => {
            let ref_struct = c
                .llvm_types
                .get_monomorphized(&MonomorphizedTypeIdentifier::new(mangled_type))
                .ok_or_else(|| format!("no LLVM type for `{mangled_type}`"))?;

            // Lifecycle is an enum with unit variants; its LLVM repr is { i8 }.
            let lifecycle_llvm = to_llvm_type(&named_std("Lifecycle"), c.context, &c.llvm_types)
                .ok_or("no LLVM type for Lifecycle enum")?;

            let fn_type = c
                .context
                .void_type()
                .fn_type(&[ref_struct.into(), lifecycle_llvm.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.register_intrinsic(
                FunctionIdentifier::new(mangled_fn),
                fn_val,
                "Ref",
                method_name,
            );

            let entry = c.context.append_basic_block(fn_val, "entry");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let pid = c
                .builder
                .build_extract_value(self_val, 0, "pid")
                .unwrap()
                .into_int_value();

            let event_val = fn_val.get_nth_param(1).unwrap().into_struct_value();
            let tag = c
                .builder
                .build_extract_value(event_val, 0, "lifecycle_tag")
                .unwrap()
                .into_int_value();

            let i64_ty = c.context.i64_type();
            let variant_i64 = c
                .builder
                .build_int_z_extend(tag, i64_ty, "variant_i64")
                .unwrap();

            let send_lifecycle_fn = *c
                .functions
                .get(&FunctionIdentifier::new("expo_rt_send_lifecycle"))
                .ok_or("expo_rt_send_lifecycle not declared")?;

            c.call_void(send_lifecycle_fn, &[pid.into(), variant_i64.into()], "");

            c.builder.build_return(None).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }

            Ok(EmitResult::Emitted)
        }
        "kill" => {
            let ref_struct = c
                .llvm_types
                .get_monomorphized(&MonomorphizedTypeIdentifier::new(mangled_type))
                .ok_or_else(|| format!("no LLVM type for `{mangled_type}`"))?;

            let fn_type = c.context.void_type().fn_type(&[ref_struct.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.register_intrinsic(
                FunctionIdentifier::new(mangled_fn),
                fn_val,
                "Ref",
                method_name,
            );

            let entry = c.context.append_basic_block(fn_val, "entry");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let pid = c
                .builder
                .build_extract_value(self_val, 0, "pid")
                .unwrap()
                .into_int_value();

            let kill_fn = *c
                .functions
                .get(&FunctionIdentifier::new("expo_rt_kill"))
                .ok_or("expo_rt_kill not declared")?;

            c.call_void(kill_fn, &[pid.into()], "");
            c.builder.build_return(None).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }

            Ok(EmitResult::Emitted)
        }
        "alive?" => {
            let ref_struct = c
                .llvm_types
                .get_monomorphized(&MonomorphizedTypeIdentifier::new(mangled_type))
                .ok_or_else(|| format!("no LLVM type for `{mangled_type}`"))?;

            let i8_ty = c.context.i8_type();
            let fn_type = i8_ty.fn_type(&[ref_struct.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.register_intrinsic(
                FunctionIdentifier::new(mangled_fn),
                fn_val,
                "Ref",
                method_name,
            );

            let entry = c.context.append_basic_block(fn_val, "entry");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let pid = c
                .builder
                .build_extract_value(self_val, 0, "pid")
                .unwrap()
                .into_int_value();

            let is_alive_fn = *c
                .functions
                .get(&FunctionIdentifier::new("expo_rt_is_process_alive"))
                .ok_or("expo_rt_is_process_alive not declared")?;

            let result_i64 = c
                .call(is_alive_fn, &[pid.into()], "alive_result")
                .ok_or("expo_rt_is_process_alive did not return a value")?
                .into_int_value();

            let i64_ty = c.context.i64_type();
            let is_alive = c
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::NE,
                    result_i64,
                    i64_ty.const_int(0, false),
                    "is_alive",
                )
                .unwrap();

            // Zero-extend i1 to i8 (Expo's Bool representation).
            let bool_val = c
                .builder
                .build_int_z_extend(is_alive, i8_ty, "bool_val")
                .unwrap();

            c.builder.build_return(Some(&bool_val)).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }

            Ok(EmitResult::Emitted)
        }
        "send_after" => {
            let ref_struct = c
                .llvm_types
                .get_monomorphized(&MonomorphizedTypeIdentifier::new(mangled_type))
                .ok_or_else(|| format!("no LLVM type for `{mangled_type}`"))?;

            let msg_type = type_args
                .first()
                .ok_or("Ref.send_after requires a type argument")?;
            let reply_type = type_args
                .get(1)
                .ok_or("Ref.send_after requires R type argument")?;
            let is_string = matches!(msg_type, Type::Primitive(Primitive::String));

            let msg_llvm = if is_string {
                c.context.ptr_type(AddressSpace::default()).into()
            } else {
                to_llvm_type(msg_type, c.context, &c.llvm_types)
                    .ok_or_else(|| format!("no LLVM type for message `{msg_type:?}`"))?
            };

            let envelope_type = process_envelope_type(msg_type, reply_type);
            ensure_types_exist(c, &envelope_type)?;
            let envelope_llvm = to_llvm_type(&envelope_type, c.context, &c.llvm_types)
                .ok_or("no LLVM type for Pair envelope")?
                .into_struct_type();

            let i64_ty = c.context.i64_type();
            let fn_type = c
                .context
                .void_type()
                .fn_type(&[ref_struct.into(), msg_llvm.into(), i64_ty.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.register_intrinsic(
                FunctionIdentifier::new(mangled_fn),
                fn_val,
                "Ref",
                method_name,
            );

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
            let delay_ms = fn_val.get_nth_param(2).unwrap();

            let option_reply_type = named_generic_std(
                "Option",
                vec![named_generic_std("ReplyTo", vec![reply_type.clone()])],
            );
            ensure_types_exist(c, &option_reply_type)?;
            let option_llvm = to_llvm_type(&option_reply_type, c.context, &c.llvm_types)
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

            let send_after_fn = *c
                .functions
                .get(&FunctionIdentifier::new("expo_rt_send_after"))
                .ok_or("expo_rt_send_after not declared")?;

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
                send_after_fn,
                &[pid.into(), msg_ptr.into(), msg_len.into(), delay_ms.into()],
                "",
            );

            c.builder.build_return(None).unwrap();

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
            let reply_to_struct = c
                .llvm_types
                .get_monomorphized(&MonomorphizedTypeIdentifier::new(mangled_type))
                .ok_or_else(|| format!("no LLVM type for `{mangled_type}`"))?;

            let reply_type = type_args
                .first()
                .ok_or("ReplyTo.send requires a type argument")?;
            let is_string = matches!(reply_type, Type::Primitive(Primitive::String));

            let reply_llvm = if is_string {
                c.context.ptr_type(AddressSpace::default()).into()
            } else {
                to_llvm_type(reply_type, c.context, &c.llvm_types)
                    .ok_or_else(|| format!("no LLVM type for reply `{reply_type:?}`"))?
            };

            let fn_type = c
                .context
                .void_type()
                .fn_type(&[reply_to_struct.into(), reply_llvm.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.register_intrinsic(
                FunctionIdentifier::new(mangled_fn),
                fn_val,
                "ReplyTo",
                method_name,
            );

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
