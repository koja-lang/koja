use inkwell::IntPredicate;
use inkwell::values::FunctionValue;

use crate::compiler::Compiler;

use super::{build_result_err, build_result_ok};

pub fn emit_fd_intrinsic<'ctx>(
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
        "Fd_read" => {
            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let fd = c.builder.build_extract_value(self_val, 0, "fd").unwrap();
            let count = fn_val.get_nth_param(1).unwrap();
            let rt_fn = *c
                .functions
                .get("expo_fd_read")
                .ok_or("expo_fd_read not declared")?;
            let ptr = c
                .call(rt_fn, &[fd.into(), count.into()], "read_ptr")
                .unwrap()
                .into_pointer_value();

            let is_null = c.builder.build_is_null(ptr, "is_null").unwrap();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_null, err_bb, ok_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);
            let ok_result = build_result_ok(c, ptr.into(), result_type);
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
        "Fd_write" => {
            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let fd = c.builder.build_extract_value(self_val, 0, "fd").unwrap();
            let data = fn_val.get_nth_param(1).unwrap();
            let rt_fn = *c
                .functions
                .get("expo_fd_write")
                .ok_or("expo_fd_write not declared")?;
            let written = c
                .call(rt_fn, &[fd.into(), data.into()], "written")
                .unwrap()
                .into_int_value();

            let neg_one = i64_ty.const_int((-1i64) as u64, true);
            let is_err = c
                .builder
                .build_int_compare(IntPredicate::EQ, written, neg_one, "is_err")
                .unwrap();

            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let err_bb = c.context.append_basic_block(fn_val, "err");
            let merge_bb = c.context.append_basic_block(fn_val, "merge");

            c.builder
                .build_conditional_branch(is_err, err_bb, ok_bb)
                .unwrap();

            c.builder.position_at_end(ok_bb);
            let ok_result = build_result_ok(c, written.into(), result_type);
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
        "Fd_close" => {
            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let fd = c.builder.build_extract_value(self_val, 0, "fd").unwrap();
            let rt_fn = *c
                .functions
                .get("expo_fd_close")
                .ok_or("expo_fd_close not declared")?;
            let ret = c
                .call(rt_fn, &[fd.into()], "close_ret")
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
            let ok_msg = c.create_string_global(b"ok", "close_ok_msg");
            let ok_result = build_result_ok(c, ok_msg.into(), result_type);
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
        _ => return Err(format!("unknown fd intrinsic: {mangled}")),
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}

/// Builds the ok/err branch for File.open: wraps an fd into a File struct Result.
fn emit_file_open_result<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    fd_val: inkwell::values::IntValue<'ctx>,
    result_type: inkwell::types::StructType<'ctx>,
) -> Result<(), String> {
    let i64_ty = c.context.i64_type();
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
    let file_struct_ty = c
        .types
        .get_stdlib("File")
        .ok_or("File struct type not found")?;
    let alloca = c.builder.build_alloca(file_struct_ty, "file_tmp").unwrap();
    let fd_field_ptr = c
        .builder
        .build_struct_gep(file_struct_ty, alloca, 0, "fd_field")
        .unwrap();
    let fd_struct_ty = c.types.get_stdlib("Fd").ok_or("Fd struct type not found")?;
    let fd_desc_ptr = c
        .builder
        .build_struct_gep(fd_struct_ty, fd_field_ptr, 0, "fd_desc")
        .unwrap();
    c.builder.build_store(fd_desc_ptr, fd_val).unwrap();
    let file_val = c
        .builder
        .build_load(file_struct_ty, alloca, "file_val")
        .unwrap();
    let ok_result = build_result_ok(c, file_val, result_type);
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
    Ok(())
}

/// Builds the ok/err branch for calls that return a pointer (null = error).
fn emit_file_ptr_result<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    runtime_fn: &str,
    args: &[inkwell::values::BasicMetadataValueEnum<'ctx>],
    result_type: inkwell::types::StructType<'ctx>,
) -> Result<(), String> {
    let rt_fn = *c
        .functions
        .get(runtime_fn)
        .ok_or(format!("{runtime_fn} not declared"))?;
    let ptr = c.call(rt_fn, args, "ptr").unwrap().into_pointer_value();
    let is_null = c.builder.build_is_null(ptr, "is_null").unwrap();

    let ok_bb = c.context.append_basic_block(fn_val, "ok");
    let err_bb = c.context.append_basic_block(fn_val, "err");
    let merge_bb = c.context.append_basic_block(fn_val, "merge");
    c.builder
        .build_conditional_branch(is_null, err_bb, ok_bb)
        .unwrap();

    c.builder.position_at_end(ok_bb);
    let ok_result = build_result_ok(c, ptr.into(), result_type);
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
    Ok(())
}

/// Builds the ok/err branch for calls that return 0 (success) or -1 (error).
/// On success, wraps an "ok" string into Result.Ok.
fn emit_file_status_result<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    runtime_fn: &str,
    args: &[inkwell::values::BasicMetadataValueEnum<'ctx>],
    result_type: inkwell::types::StructType<'ctx>,
) -> Result<(), String> {
    let i64_ty = c.context.i64_type();
    let rt_fn = *c
        .functions
        .get(runtime_fn)
        .ok_or(format!("{runtime_fn} not declared"))?;
    let ret = c.call(rt_fn, args, "ret").unwrap().into_int_value();
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
    let ok_str = c.create_string_global(b"ok", "ok_str");
    let ok_result = build_result_ok(c, ok_str.into(), result_type);
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
    Ok(())
}

pub fn emit_file_intrinsic<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    mangled: &str,
) -> Result<(), String> {
    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    let i64_ty = c.context.i64_type();

    match mangled {
        "File_open" => {
            let result_type = fn_val
                .get_type()
                .get_return_type()
                .unwrap()
                .into_struct_type();
            let path_ptr = fn_val.get_nth_param(0).unwrap();
            let mode_enum = fn_val.get_nth_param(1).unwrap().into_struct_value();
            let mode_tag = c
                .builder
                .build_extract_value(mode_enum, 0, "mode_tag")
                .unwrap()
                .into_int_value();
            let mode = c
                .builder
                .build_int_z_extend(mode_tag, i64_ty, "mode")
                .unwrap();
            let rt_fn = *c
                .functions
                .get("expo_file_open")
                .ok_or("expo_file_open not declared")?;
            let fd_val = c
                .call(rt_fn, &[path_ptr.into(), mode.into()], "fd_val")
                .unwrap()
                .into_int_value();

            emit_file_open_result(c, fn_val, fd_val, result_type)?;
        }
        "File_read" => {
            let result_type = fn_val
                .get_type()
                .get_return_type()
                .unwrap()
                .into_struct_type();
            let path_ptr = fn_val.get_nth_param(0).unwrap();
            emit_file_ptr_result(
                c,
                fn_val,
                "expo_file_read_all",
                &[path_ptr.into()],
                result_type,
            )?;
        }
        "File_write" => {
            let result_type = fn_val
                .get_type()
                .get_return_type()
                .unwrap()
                .into_struct_type();
            let path_ptr = fn_val.get_nth_param(0).unwrap();
            let content_ptr = fn_val.get_nth_param(1).unwrap();
            emit_file_status_result(
                c,
                fn_val,
                "expo_file_write_all",
                &[path_ptr.into(), content_ptr.into()],
                result_type,
            )?;
        }
        "File_exists?" => {
            let path_ptr = fn_val.get_nth_param(0).unwrap();
            let rt_fn = *c
                .functions
                .get("expo_file_exists")
                .ok_or("expo_file_exists not declared")?;
            let raw = c
                .call(rt_fn, &[path_ptr.into()], "exists_raw")
                .unwrap()
                .into_int_value();
            let one = i64_ty.const_int(1, false);
            let result = c
                .builder
                .build_int_compare(IntPredicate::EQ, raw, one, "exists")
                .unwrap();
            c.builder.build_return(Some(&result)).unwrap();
        }
        "File_delete" => {
            let result_type = fn_val
                .get_type()
                .get_return_type()
                .unwrap()
                .into_struct_type();
            let path_ptr = fn_val.get_nth_param(0).unwrap();
            emit_file_status_result(
                c,
                fn_val,
                "expo_file_delete",
                &[path_ptr.into()],
                result_type,
            )?;
        }
        "File_rename" => {
            let result_type = fn_val
                .get_type()
                .get_return_type()
                .unwrap()
                .into_struct_type();
            let src_ptr = fn_val.get_nth_param(0).unwrap();
            let dst_ptr = fn_val.get_nth_param(1).unwrap();
            emit_file_status_result(
                c,
                fn_val,
                "expo_file_rename",
                &[src_ptr.into(), dst_ptr.into()],
                result_type,
            )?;
        }
        _ => return Err(format!("unknown file intrinsic: {mangled}")),
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}
