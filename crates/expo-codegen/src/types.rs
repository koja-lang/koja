use std::collections::HashMap;

use expo_typecheck::types::{Primitive, Type};
use inkwell::context::Context;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, StructType};

pub fn to_llvm_type<'ctx>(
    ty: &Type,
    context: &'ctx Context,
    struct_types: &HashMap<String, StructType<'ctx>>,
) -> Option<BasicTypeEnum<'ctx>> {
    match ty {
        Type::Primitive(p) => Some(primitive_to_llvm(p, context)),
        Type::Struct(name) | Type::Enum(name) => struct_types.get(name).map(|st| (*st).into()),
        Type::Function { .. } => Some(context.ptr_type(inkwell::AddressSpace::default()).into()),
        Type::Unit => None,
        _ => None,
    }
}

pub fn to_llvm_metadata_type<'ctx>(
    ty: &Type,
    context: &'ctx Context,
    struct_types: &HashMap<String, StructType<'ctx>>,
) -> Option<BasicMetadataTypeEnum<'ctx>> {
    to_llvm_type(ty, context, struct_types).map(|t| t.into())
}

pub fn primitive_to_llvm<'ctx>(p: &Primitive, context: &'ctx Context) -> BasicTypeEnum<'ctx> {
    match p {
        Primitive::Bool => context.bool_type().into(),
        Primitive::F32 => context.f32_type().into(),
        Primitive::F64 => context.f64_type().into(),
        Primitive::I8 | Primitive::U8 => context.i8_type().into(),
        Primitive::I16 | Primitive::U16 => context.i16_type().into(),
        Primitive::I32 | Primitive::U32 => context.i32_type().into(),
        Primitive::I64 | Primitive::U64 => context.i64_type().into(),
        Primitive::String => context.ptr_type(inkwell::AddressSpace::default()).into(),
    }
}
