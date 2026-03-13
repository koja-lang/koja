use expo_ast::ast::EnumConstructionData;
use inkwell::values::{BasicValueEnum, FunctionValue};

use crate::compiler::Compiler;
use crate::expr::compile_expr;

pub fn compile_enum_construction<'ctx>(
    c: &mut Compiler<'ctx>,
    type_path: &[String],
    variant: &str,
    data: &EnumConstructionData,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let enum_name = type_path
        .first()
        .ok_or("empty type path in enum construction")?;

    let enum_type = *c
        .struct_types
        .get(enum_name)
        .ok_or_else(|| format!("unknown enum type: {enum_name}"))?;

    let tag = c
        .get_variant_tag(enum_name, variant)
        .ok_or_else(|| format!("unknown variant `{variant}` on enum `{enum_name}`"))?;

    let alloca = c
        .builder
        .build_alloca(enum_type, &format!("{enum_name}_{variant}"))
        .unwrap();

    let tag_ptr = c
        .builder
        .build_struct_gep(enum_type, alloca, 0, "tag_ptr")
        .unwrap();
    let tag_val = c.context.i8_type().const_int(tag as u64, false);
    c.builder.build_store(tag_ptr, tag_val).unwrap();

    match data {
        EnumConstructionData::Unit => {}
        EnumConstructionData::Tuple(exprs) => {
            let payload_type = c
                .get_variant_payload_type(enum_name, variant)
                .ok_or_else(|| format!("no payload type for {enum_name}.{variant}"))?;

            let payload_ptr = c
                .builder
                .build_struct_gep(enum_type, alloca, 1, "payload_ptr")
                .unwrap();

            for (i, expr) in exprs.iter().enumerate() {
                let val = compile_expr(c, expr, function)?
                    .ok_or_else(|| format!("enum field {i} produced no value"))?;
                let field_ptr = c
                    .builder
                    .build_struct_gep(payload_type, payload_ptr, i as u32, &format!("field_{i}"))
                    .unwrap();
                c.builder.build_store(field_ptr, val).unwrap();
            }
        }
        EnumConstructionData::Struct(fields) => {
            let payload_type = c
                .get_variant_payload_type(enum_name, variant)
                .ok_or_else(|| format!("no payload type for {enum_name}.{variant}"))?;

            let payload_ptr = c
                .builder
                .build_struct_gep(enum_type, alloca, 1, "payload_ptr")
                .unwrap();

            let variant_info = c
                .type_ctx
                .enums
                .get(enum_name)
                .and_then(|ei| ei.variants.iter().find(|v| v.name == variant))
                .ok_or_else(|| format!("variant info not found for {enum_name}.{variant}"))?;

            let expected_fields = match &variant_info.data {
                expo_typecheck::context::VariantData::Struct(f) => f,
                _ => return Err(format!("{enum_name}.{variant} is not a struct variant")),
            };

            for field_init in fields {
                let field_idx = expected_fields
                    .iter()
                    .position(|(name, _)| *name == field_init.name)
                    .ok_or_else(|| {
                        format!(
                            "unknown field `{}` in {enum_name}.{variant}",
                            field_init.name
                        )
                    })? as u32;

                let val = compile_expr(c, &field_init.value, function)?
                    .ok_or_else(|| format!("field `{}` produced no value", field_init.name))?;
                let field_ptr = c
                    .builder
                    .build_struct_gep(payload_type, payload_ptr, field_idx, &field_init.name)
                    .unwrap();
                c.builder.build_store(field_ptr, val).unwrap();
            }
        }
    }

    let enum_val = c.builder.build_load(enum_type, alloca, enum_name).unwrap();
    Ok(Some(enum_val))
}
