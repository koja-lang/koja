use expo_ast::ast::{Item, Module, Param};

use crate::context::{FunctionSig, ParamInfo, StructInfo, TypeContext};
use crate::types::{Type, resolve_type_expr};

pub fn collect(module: &Module) -> TypeContext {
    let mut ctx = TypeContext::new();

    let struct_names: Vec<&str> = module
        .items
        .iter()
        .filter_map(|item| {
            if let Item::Struct(s) = item {
                Some(s.name.as_str())
            } else {
                None
            }
        })
        .collect();

    for item in &module.items {
        match item {
            Item::Struct(s) => {
                if !s.type_params.is_empty() {
                    continue;
                }
                let fields: Vec<(String, Type)> = s
                    .fields
                    .iter()
                    .map(|f| {
                        let ty = resolve_type_expr(&f.type_expr, &struct_names);
                        (f.name.clone(), ty)
                    })
                    .collect();
                ctx.structs.insert(
                    s.name.clone(),
                    StructInfo {
                        fields,
                        span: s.span,
                    },
                );
            }
            Item::Function(f) => {
                collect_function(&mut ctx, f, &struct_names);
            }
            _ => {}
        }
    }

    ctx
}

fn collect_function(
    ctx: &mut TypeContext,
    f: &expo_ast::ast::Function,
    known_structs: &[&str],
) {
    if !f.type_params.is_empty() {
        return;
    }

    let params: Vec<ParamInfo> = f
        .params
        .iter()
        .filter_map(|p| match p {
            Param::Regular {
                name, type_expr, ..
            } => {
                let ty = resolve_type_expr(type_expr, known_structs);
                Some(ParamInfo {
                    name: name.clone(),
                    ty,
                })
            }
            Param::Self_ { .. } => None,
        })
        .collect();

    let return_type = f
        .return_type
        .as_ref()
        .map(|t| resolve_type_expr(t, known_structs))
        .unwrap_or(Type::Unit);

    ctx.functions.insert(
        f.name.clone(),
        FunctionSig {
            params,
            return_type,
            span: f.span,
        },
    );
}
