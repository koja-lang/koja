//! Type mapping: converts Expo types to their LLVM representations (basic
//! types, metadata types, and struct type lookups).

use std::collections::HashMap;

use expo_typecheck::types::{Primitive, Type, mangle_name, mangle_type, named};
use inkwell::context::Context;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, StructType};

/// Converts an Expo type to an LLVM basic type. Returns `None` for `Unit`
/// and other types without an LLVM representation.
pub fn to_llvm_type<'ctx>(
    ty: &Type,
    context: &'ctx Context,
    structs: &HashMap<String, StructType<'ctx>>,
) -> Option<BasicTypeEnum<'ctx>> {
    match ty {
        Type::Indirect(_) => Some(context.ptr_type(inkwell::AddressSpace::default()).into()),
        Type::Primitive(primitive) => Some(primitive_to_llvm(primitive, context)),
        Type::Named {
            identifier,
            type_args,
        } => {
            if type_args.is_empty() {
                structs.get(&identifier.name).map(|st| (*st).into())
            } else {
                let mangled = mangle_name(&identifier.name, type_args);
                structs.get(&mangled).map(|st| (*st).into())
            }
        }
        Type::Union(_) => {
            let mangled = mangle_type(ty);
            structs.get(&mangled).map(|st| (*st).into())
        }
        Type::Function { .. } => {
            let ptr_type = context.ptr_type(inkwell::AddressSpace::default());
            Some(
                context
                    .struct_type(&[ptr_type.into(), ptr_type.into()], false)
                    .into(),
            )
        }
        Type::Unit => None,
        _ => None,
    }
}

/// Wraps [`to_llvm_type`] to return a `BasicMetadataTypeEnum` for use in
/// function signatures.
pub fn to_llvm_metadata_type<'ctx>(
    ty: &Type,
    context: &'ctx Context,
    structs: &HashMap<String, StructType<'ctx>>,
) -> Option<BasicMetadataTypeEnum<'ctx>> {
    to_llvm_type(ty, context, structs).map(|t| t.into())
}

/// Converts a type name like `"Int32"` or `"String"` to its Expo `Type`.
/// Falls back to `Type::Struct` for unrecognised names.
pub fn primitive_name_to_type(name: &str) -> Type {
    match name {
        "Binary" => Type::Primitive(Primitive::Binary),
        "Bits" => Type::Primitive(Primitive::Bits),
        "Bool" => Type::Primitive(Primitive::Bool),
        "Int" => Type::Primitive(Primitive::I64),
        "Int8" => Type::Primitive(Primitive::I8),
        "Int16" => Type::Primitive(Primitive::I16),
        "Int32" => Type::Primitive(Primitive::I32),
        "UInt8" => Type::Primitive(Primitive::U8),
        "UInt16" => Type::Primitive(Primitive::U16),
        "UInt32" => Type::Primitive(Primitive::U32),
        "UInt64" => Type::Primitive(Primitive::U64),
        "String" => Type::Primitive(Primitive::String),
        "Float" => Type::Primitive(Primitive::F64),
        "Float32" => Type::Primitive(Primitive::F32),
        _ => named(name),
    }
}

/// Maps an Expo primitive type to its corresponding LLVM type.
pub fn primitive_to_llvm<'ctx>(
    primitive: &Primitive,
    context: &'ctx Context,
) -> BasicTypeEnum<'ctx> {
    match primitive {
        Primitive::Binary | Primitive::Bits | Primitive::String => {
            context.ptr_type(inkwell::AddressSpace::default()).into()
        }
        Primitive::Bool => context.bool_type().into(),
        Primitive::F32 => context.f32_type().into(),
        Primitive::F64 => context.f64_type().into(),
        Primitive::I8 | Primitive::U8 => context.i8_type().into(),
        Primitive::I16 | Primitive::U16 => context.i16_type().into(),
        Primitive::I32 | Primitive::U32 => context.i32_type().into(),
        Primitive::I64 | Primitive::U64 => context.i64_type().into(),
    }
}
