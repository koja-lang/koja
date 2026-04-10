use inkwell::values::FunctionValue;

use crate::compiler::Compiler;

pub fn emit_random_intrinsic<'ctx>(
    c: &mut Compiler<'ctx>,
    fn_val: FunctionValue<'ctx>,
    mangled: &str,
) -> Result<(), String> {
    let entry = c.context.append_basic_block(fn_val, "entry");
    let saved_block = c.builder.get_insert_block();
    c.builder.position_at_end(entry);

    match mangled {
        "Random_bytes" => {
            let count = fn_val.get_nth_param(0).unwrap();
            let rt_fn = *c
                .functions
                .get("expo_random_bytes")
                .ok_or("expo_random_bytes not declared")?;
            let bin_ptr = c.call(rt_fn, &[count.into()], "bin_ptr").unwrap();
            c.builder.build_return(Some(&bin_ptr)).unwrap();
        }
        _ => return Err(format!("unknown random intrinsic: {mangled}")),
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}
