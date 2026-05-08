//! Plain-struct destructure pattern lowering.
//!
//! Always a [`PatternCheck::CatchAll`] — the IR emits no tag check,
//! only the per-field [`BindSource::StructField`] binds for named
//! [`Pattern::Binding`] fields. Wildcards are skipped, mirroring
//! the enum-payload path.

use expo_ast::ast::{FieldPattern, Pattern};
use expo_ast::identifier::Resolution;

use super::super::ctx::{FnLowerCtx, LowerOutput};
use super::super::structs::{resolved_struct_symbol, struct_definition_from_resolution};
use super::{
    BindSource, PatternCheck, PatternInputs, PayloadBind, ensure_local_declared, field_type_for,
    require_local,
};

pub(super) fn lower_struct_check(
    fields: &[FieldPattern],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    output: &mut LowerOutput,
) -> PatternCheck {
    let binds = build_struct_field_binds(fields, inputs, ctx, output);
    PatternCheck::CatchAll { binds }
}

fn build_struct_field_binds(
    fields: &[FieldPattern],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    output: &mut LowerOutput,
) -> Vec<PayloadBind> {
    let definition =
        struct_definition_from_resolution(inputs.subject_ty, inputs.registry, "struct pattern");
    let struct_symbol = resolved_struct_symbol(
        inputs.subject_ty,
        inputs.registry,
        &mut output.instantiations,
    );
    let owner = match inputs.subject_ty.resolution {
        Resolution::Global(id) => id,
        _ => panic!(
            "alpha IR lower: struct pattern subject has non-Global resolution after \
             typecheck seal",
        ),
    };
    let mut binds = Vec::new();
    for field in fields {
        let Pattern::Binding { local_id, name, .. } = &field.pattern else {
            continue;
        };
        let (field_index, declared) = definition.lookup_field(&field.name).unwrap_or_else(|| {
            panic!(
                "alpha IR lower: struct pattern references unknown field `{}` — \
                     typecheck invariant violation",
                field.name,
            )
        });
        let ir_local = require_local(*local_id, name);
        let field_type = field_type_for(&declared.ty, owner, inputs, output);
        ensure_local_declared(ir_local, &field_type, ctx);
        binds.push(PayloadBind {
            field_type,
            local: ir_local,
            source: BindSource::StructField {
                field_index,
                struct_symbol: struct_symbol.clone(),
            },
        });
    }
    binds
}
