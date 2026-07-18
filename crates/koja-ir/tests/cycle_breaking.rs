use koja_ir::{IRType, IRVariantPayload};

mod common;

use common::lower_program_source as lower;

fn is_indirect(ty: &IRType) -> bool {
    matches!(ty, IRType::Indirect(_))
}

#[test]
fn recursive_type_graph_inserts_indirection() {
    let program = lower(
        "
        struct Node
          next: Option<Node>
        end
        ",
    );

    let struct_has_indirection = program
        .packages
        .iter()
        .flat_map(|package| package.structs.values())
        .flat_map(|declaration| &declaration.fields)
        .any(|field| is_indirect(&field.ir_type));
    let enum_has_indirection = program
        .packages
        .iter()
        .flat_map(|package| package.enums.values())
        .flat_map(|declaration| &declaration.variants)
        .any(|variant| match &variant.payload {
            IRVariantPayload::Struct(fields) => {
                fields.iter().any(|field| is_indirect(&field.ir_type))
            }
            IRVariantPayload::Tuple(types) => types.iter().any(is_indirect),
            IRVariantPayload::Unit => false,
        });

    assert!(struct_has_indirection || enum_has_indirection);
}
