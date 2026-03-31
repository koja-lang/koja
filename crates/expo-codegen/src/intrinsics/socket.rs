use inkwell::IntPredicate;
use inkwell::values::FunctionValue;

use crate::compiler::Compiler;

use super::{build_result_err, build_result_ok};

pub fn emit_socket_intrinsic<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    mangled: &str,
) -> Result<(), String> {
    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    let i64_ty = c.context.i64_type();
    let result_type = fn_val
        .get_type()
        .get_return_type()
        .unwrap()
        .into_struct_type();

    match mangled {
        "Socket_create" => {
            let kind_enum = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let kind_tag = c
                .builder
                .build_extract_value(kind_enum, 0, "kind_tag")
                .unwrap()
                .into_int_value();
            let kind = c
                .builder
                .build_int_z_extend(kind_tag, i64_ty, "kind")
                .unwrap();

            let rt_fn = *c
                .functions
                .get("expo_socket_create")
                .ok_or("expo_socket_create not declared")?;
            let fd_val = c
                .call(rt_fn, &[kind.into()], "fd_val")
                .unwrap()
                .into_int_value();

            let neg_one = i64_ty.const_int((-1i64) as u64, true);
            let is_err = c
                .builder
                .build_int_compare(IntPredicate::EQ, fd_val, neg_one, "is_err")
                .unwrap();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_err, err_bb, ok_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);
            let socket_struct_ty = c
                .types
                .structs
                .get("Socket")
                .copied()
                .ok_or("Socket struct type not found")?;
            let alloca = c
                .builder
                .build_alloca(socket_struct_ty, "sock_tmp")
                .unwrap();
            let fd_field_ptr = c
                .builder
                .build_struct_gep(socket_struct_ty, alloca, 0, "fd_field")
                .unwrap();
            let fd_struct_ty = c
                .types
                .structs
                .get("Fd")
                .copied()
                .ok_or("Fd struct type not found")?;
            let fd_desc_ptr = c
                .builder
                .build_struct_gep(fd_struct_ty, fd_field_ptr, 0, "fd_desc")
                .unwrap();
            c.builder.build_store(fd_desc_ptr, fd_val).unwrap();
            let sock_val = c
                .builder
                .build_load(socket_struct_ty, alloca, "sock_val")
                .unwrap();
            let ok_result = build_result_ok(c, sock_val, result_type);
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
        "Socket_bind" | "Socket_connect" | "Socket_listen" | "Socket_set_reuse_addr" => {
            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let fd_inner = c
                .builder
                .build_extract_value(self_val, 0, "fd_struct")
                .unwrap();
            let fd = c
                .builder
                .build_extract_value(fd_inner.into_struct_value(), 0, "fd")
                .unwrap();

            let (rt_name, args): (&str, Vec<inkwell::values::BasicMetadataValueEnum>) =
                match mangled {
                    "Socket_bind" | "Socket_connect" => {
                        let addr_val = fn_val.get_nth_param(1).unwrap().into_struct_value();
                        let ip_struct = c
                            .builder
                            .build_extract_value(addr_val, 0, "ip_struct")
                            .unwrap();
                        let ip_bytes = c
                            .builder
                            .build_extract_value(ip_struct.into_struct_value(), 0, "ip_bytes")
                            .unwrap();
                        let port = c.builder.build_extract_value(addr_val, 1, "port").unwrap();
                        let rt = if mangled == "Socket_bind" {
                            "expo_socket_bind"
                        } else {
                            "expo_socket_connect"
                        };
                        (rt, vec![fd.into(), ip_bytes.into(), port.into()])
                    }
                    "Socket_listen" => {
                        let backlog = fn_val.get_nth_param(1).unwrap();
                        ("expo_socket_listen", vec![fd.into(), backlog.into()])
                    }
                    "Socket_set_reuse_addr" => ("expo_socket_setsockopt_reuse", vec![fd.into()]),
                    _ => unreachable!(),
                };

            let rt_fn = *c
                .functions
                .get(rt_name)
                .ok_or(format!("{rt_name} not declared"))?;
            let ret = c.call(rt_fn, &args, "ret").unwrap().into_int_value();

            let neg_one = i64_ty.const_int((-1i64) as u64, true);
            let is_err = c
                .builder
                .build_int_compare(IntPredicate::EQ, ret, neg_one, "is_err")
                .unwrap();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_err, err_bb, ok_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);
            let ok_result = build_result_ok(c, self_val.into(), result_type);
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
        "Socket_accept" => {
            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let fd_inner = c
                .builder
                .build_extract_value(self_val, 0, "fd_struct")
                .unwrap();
            let fd = c
                .builder
                .build_extract_value(fd_inner.into_struct_value(), 0, "fd")
                .unwrap();

            let rt_fn = *c
                .functions
                .get("expo_socket_accept")
                .ok_or("expo_socket_accept not declared")?;
            let client_fd = c
                .call(rt_fn, &[fd.into()], "client_fd")
                .unwrap()
                .into_int_value();

            let neg_one = i64_ty.const_int((-1i64) as u64, true);
            let is_err = c
                .builder
                .build_int_compare(IntPredicate::EQ, client_fd, neg_one, "is_err")
                .unwrap();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_err, err_bb, ok_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);
            let fd_struct_ty = c
                .types
                .structs
                .get("Fd")
                .copied()
                .ok_or("Fd struct type not found")?;
            let alloca = c.builder.build_alloca(fd_struct_ty, "fd_tmp").unwrap();
            let fd_desc_ptr = c
                .builder
                .build_struct_gep(fd_struct_ty, alloca, 0, "fd_desc")
                .unwrap();
            c.builder.build_store(fd_desc_ptr, client_fd).unwrap();
            let fd_val = c
                .builder
                .build_load(fd_struct_ty, alloca, "fd_val")
                .unwrap();
            let ok_result = build_result_ok(c, fd_val, result_type);
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
        "Socket_resolve" => {
            let hostname_ptr = fn_val.get_nth_param(0).unwrap();
            let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());

            let rt_fn = *c
                .functions
                .get("expo_socket_resolve")
                .ok_or("expo_socket_resolve not declared")?;
            let result_ptr = c
                .call(rt_fn, &[hostname_ptr.into()], "resolve_buf")
                .unwrap()
                .into_pointer_value();

            let null_ptr = ptr_ty.const_null();
            let is_null = c
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    c.builder
                        .build_ptr_to_int(result_ptr, i64_ty, "ptr_int")
                        .unwrap(),
                    c.builder
                        .build_ptr_to_int(null_ptr, i64_ty, "null_int")
                        .unwrap(),
                    "is_null",
                )
                .unwrap();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_null, err_bb, ok_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);

            let count = c
                .builder
                .build_load(i64_ty, result_ptr, "count")
                .unwrap()
                .into_int_value();

            let list_type_name = "List_$IPAddress$";
            let list_struct = *c
                .types
                .structs
                .get(list_type_name)
                .ok_or(format!("{list_type_name} struct type not found"))?;

            let ip_struct_ty = c
                .types
                .structs
                .get("IPAddress")
                .copied()
                .ok_or("IPAddress struct type not found")?;
            let ip_size = crate::compiler::llvm_field_byte_size(ip_struct_ty.into()) as u64;
            let alloc_size = c
                .builder
                .build_int_mul(count, i64_ty.const_int(ip_size, false), "alloc_sz")
                .unwrap();
            let malloc_fn = *c.functions.get("malloc").ok_or("malloc not declared")?;
            let list_buf = c
                .call(malloc_fn, &[alloc_size.into()], "list_buf")
                .unwrap()
                .into_pointer_value();

            let i8_ty = c.context.i8_type();
            let ptrs_start = unsafe {
                c.builder
                    .build_gep(
                        i8_ty,
                        result_ptr,
                        &[i64_ty.const_int(8, false)],
                        "ptrs_start",
                    )
                    .unwrap()
            };
            let memcpy_fn = *c.functions.get("memcpy").ok_or("memcpy not declared")?;
            c.call_void(
                memcpy_fn,
                &[list_buf.into(), ptrs_start.into(), alloc_size.into()],
                "cpy",
            );

            let free_fn = *c.functions.get("free").ok_or("free not declared")?;
            c.call_void(free_fn, &[result_ptr.into()], "free_buf");

            let list_val = list_struct.get_undef();
            let list_val = c
                .builder
                .build_insert_value(list_val, list_buf, 0, "with_ptr")
                .unwrap()
                .into_struct_value();
            let list_val = c
                .builder
                .build_insert_value(list_val, count, 1, "with_len")
                .unwrap()
                .into_struct_value();
            let list_val = c
                .builder
                .build_insert_value(list_val, count, 2, "with_cap")
                .unwrap()
                .into_struct_value();

            let ok_result = build_result_ok(c, list_val.into(), result_type);
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
        "Socket_send_to" => {
            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let fd_inner = c
                .builder
                .build_extract_value(self_val, 0, "fd_struct")
                .unwrap();
            let fd = c
                .builder
                .build_extract_value(fd_inner.into_struct_value(), 0, "fd")
                .unwrap();

            let data_ptr = fn_val.get_nth_param(1).unwrap();
            let addr_val = fn_val.get_nth_param(2).unwrap().into_struct_value();
            let ip_struct = c
                .builder
                .build_extract_value(addr_val, 0, "ip_struct")
                .unwrap();
            let ip_bytes = c
                .builder
                .build_extract_value(ip_struct.into_struct_value(), 0, "ip_bytes")
                .unwrap();
            let port = c.builder.build_extract_value(addr_val, 1, "port").unwrap();

            let rt_fn = *c
                .functions
                .get("expo_socket_send_to")
                .ok_or("expo_socket_send_to not declared")?;
            let ret = c
                .call(
                    rt_fn,
                    &[fd.into(), data_ptr.into(), ip_bytes.into(), port.into()],
                    "sent",
                )
                .unwrap()
                .into_int_value();

            let neg_one = i64_ty.const_int((-1i64) as u64, true);
            let is_err = c
                .builder
                .build_int_compare(IntPredicate::EQ, ret, neg_one, "is_err")
                .unwrap();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_err, err_bb, ok_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);
            let ok_result = build_result_ok(c, ret.into(), result_type);
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
        "Socket_recv_from" => {
            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let fd_inner = c
                .builder
                .build_extract_value(self_val, 0, "fd_struct")
                .unwrap();
            let fd = c
                .builder
                .build_extract_value(fd_inner.into_struct_value(), 0, "fd")
                .unwrap();
            let count_val = fn_val.get_nth_param(1).unwrap();

            let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());

            let rt_fn = *c
                .functions
                .get("expo_socket_recv_from")
                .ok_or("expo_socket_recv_from not declared")?;
            let result_ptr = c
                .call(rt_fn, &[fd.into(), count_val.into()], "recv_buf")
                .unwrap()
                .into_pointer_value();

            let null_ptr = ptr_ty.const_null();
            let is_null = c
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    c.builder
                        .build_ptr_to_int(result_ptr, i64_ty, "ptr_int")
                        .unwrap(),
                    c.builder
                        .build_ptr_to_int(null_ptr, i64_ty, "null_int")
                        .unwrap(),
                    "is_null",
                )
                .unwrap();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_null, err_bb, ok_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);

            let data_ptr = c
                .builder
                .build_load(ptr_ty, result_ptr, "data_ptr")
                .unwrap();

            let i8_ty = c.context.i8_type();
            let ip_field_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, result_ptr, &[i64_ty.const_int(8, false)], "ip_field")
                    .unwrap()
            };
            let ip_bin_ptr = c
                .builder
                .build_load(ptr_ty, ip_field_ptr, "ip_bin")
                .unwrap();

            let port_field_ptr = unsafe {
                c.builder
                    .build_gep(
                        i8_ty,
                        result_ptr,
                        &[i64_ty.const_int(16, false)],
                        "port_field",
                    )
                    .unwrap()
            };
            let recv_port = c
                .builder
                .build_load(i64_ty, port_field_ptr, "port")
                .unwrap();

            let free_fn = *c.functions.get("free").ok_or("free not declared")?;
            c.call_void(free_fn, &[result_ptr.into()], "free_buf");

            let ip_struct_ty = c
                .types
                .structs
                .get("IPAddress")
                .copied()
                .ok_or("IPAddress struct type not found")?;
            let ip_val = ip_struct_ty.get_undef();
            let ip_val = c
                .builder
                .build_insert_value(ip_val, ip_bin_ptr, 0, "ip_with_bytes")
                .unwrap()
                .into_struct_value();

            let sa_struct_ty = c
                .types
                .structs
                .get("SocketAddress")
                .copied()
                .ok_or("SocketAddress struct type not found")?;
            let sa_val = sa_struct_ty.get_undef();
            let sa_val = c
                .builder
                .build_insert_value(sa_val, ip_val, 0, "sa_with_ip")
                .unwrap()
                .into_struct_value();
            let sa_val = c
                .builder
                .build_insert_value(sa_val, recv_port, 1, "sa_with_port")
                .unwrap()
                .into_struct_value();

            let pair_type_name = "Pair_$String.SocketAddress$";
            let pair_struct = *c
                .types
                .structs
                .get(pair_type_name)
                .ok_or(format!("{pair_type_name} struct type not found"))?;
            let pair_val = pair_struct.get_undef();
            let pair_val = c
                .builder
                .build_insert_value(pair_val, data_ptr, 0, "pair_with_data")
                .unwrap()
                .into_struct_value();
            let pair_val = c
                .builder
                .build_insert_value(pair_val, sa_val, 1, "pair_with_addr")
                .unwrap()
                .into_struct_value();

            let ok_result = build_result_ok(c, pair_val.into(), result_type);
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
        _ => return Err(format!("unknown socket intrinsic: {mangled}")),
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}
