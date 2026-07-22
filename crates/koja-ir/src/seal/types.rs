//! Typed IR invariants shared by program and script sealing.
//!
//! The structural seal proves that every value exists. This pass also
//! proves that each use agrees with the producer's concrete IR type.

use std::collections::BTreeMap;

use crate::constant::IRConstantValue;
use crate::enum_decl::{EnumPayloadInit, IREnumDecl, IRVariantPayload};
use crate::function::{
    FunctionKind, IRBasicBlock, IRBlockId, IRFunction, IRFunctionParam, IRInstruction, IRSymbol,
    IRTerminator,
};
use crate::local::IRLocalId;
use crate::package::IRPackage;
use crate::struct_decl::IRStructDecl;
use crate::types::{
    ConstValue, IRBinOp, IRType, IRUnaryOp, LoweredBinaryPattern, LoweredBinarySegment, ValueId,
};
use crate::union_decl::IRUnionDecl;

use super::seal_panic;

/// Read-only declaration indices over a sealed program's package set.
pub(super) struct TypeEnvironment<'a> {
    constants: BTreeMap<String, &'a IRConstantValue>,
    enums: BTreeMap<String, &'a IREnumDecl>,
    functions: BTreeMap<String, &'a IRFunction>,
    structs: BTreeMap<String, &'a IRStructDecl>,
    unions: BTreeMap<String, &'a IRUnionDecl>,
}

impl<'a> TypeEnvironment<'a> {
    pub(super) fn new(packages: &'a [IRPackage]) -> Self {
        let mut environment = Self {
            constants: BTreeMap::new(),
            enums: BTreeMap::new(),
            functions: BTreeMap::new(),
            structs: BTreeMap::new(),
            unions: BTreeMap::new(),
        };
        for package in packages {
            insert_unique(&mut environment.constants, &package.constants, "constant");
            insert_unique(&mut environment.enums, &package.enums, "enum");
            insert_unique(&mut environment.functions, &package.functions, "function");
            insert_unique(&mut environment.structs, &package.structs, "struct");
            insert_unique(&mut environment.unions, &package.unions, "union");
        }
        environment
    }

    fn constant(&self, symbol: &IRSymbol) -> &'a IRConstantValue {
        self.constants
            .get(symbol.mangled())
            .copied()
            .unwrap_or_else(|| {
                seal_panic(&format!(
                    "constant `{symbol}` is not registered in any package"
                ))
            })
    }

    fn enum_decl(&self, symbol: &IRSymbol) -> &'a IREnumDecl {
        self.enums
            .get(symbol.mangled())
            .copied()
            .unwrap_or_else(|| {
                seal_panic(&format!("enum `{symbol}` is not registered in any package"))
            })
    }

    fn function(&self, symbol: &IRSymbol) -> &'a IRFunction {
        self.functions
            .get(symbol.mangled())
            .copied()
            .unwrap_or_else(|| {
                seal_panic(&format!(
                    "function `{symbol}` is not registered in any package"
                ))
            })
    }

    fn struct_decl(&self, symbol: &IRSymbol) -> &'a IRStructDecl {
        self.structs
            .get(symbol.mangled())
            .copied()
            .unwrap_or_else(|| {
                seal_panic(&format!(
                    "struct `{symbol}` is not registered in any package"
                ))
            })
    }

    fn union_decl(&self, symbol: &IRSymbol) -> &'a IRUnionDecl {
        self.unions
            .get(symbol.mangled())
            .copied()
            .unwrap_or_else(|| {
                seal_panic(&format!(
                    "union `{symbol}` is not registered in any package"
                ))
            })
    }
}

fn insert_unique<'a, T>(
    output: &mut BTreeMap<String, &'a T>,
    declarations: &'a BTreeMap<IRSymbol, T>,
    kind: &str,
) {
    for (symbol, declaration) in declarations {
        if output
            .insert(symbol.mangled().to_string(), declaration)
            .is_some()
        {
            seal_panic(&format!(
                "{kind} symbol `{symbol}` is registered in more than one package"
            ));
        }
    }
}

pub(super) fn seal_package_types(package: &IRPackage, environment: &TypeEnvironment<'_>) {
    for function in package.functions.values() {
        seal_function_types(function, environment);
    }
}

pub(super) fn seal_script_body_types(blocks: &[IRBasicBlock], environment: &TypeEnvironment<'_>) {
    seal_body_types(blocks, &[], "script body", environment, None);
}

fn seal_function_types(function: &IRFunction, environment: &TypeEnvironment<'_>) {
    if function.blocks.is_empty() {
        return;
    }
    let owner = format!("function `{}`", function.symbol);
    seal_body_types(
        &function.blocks,
        &function.params,
        &owner,
        environment,
        Some(function),
    );
}

fn seal_body_types(
    blocks: &[IRBasicBlock],
    params: &[IRFunctionParam],
    owner: &str,
    environment: &TypeEnvironment<'_>,
    function: Option<&IRFunction>,
) {
    let local_types = collect_local_types(blocks, params, owner);
    let value_types = collect_value_types(blocks, params, owner, environment);

    for block in blocks {
        for instruction in &block.instructions {
            seal_instruction_types(
                instruction,
                &value_types,
                &local_types,
                owner,
                environment,
                function,
            );
        }
        seal_terminator_types(&block.terminator, blocks, &value_types, params, owner);
    }
}

fn collect_local_types(
    blocks: &[IRBasicBlock],
    params: &[IRFunctionParam],
    owner: &str,
) -> BTreeMap<IRLocalId, IRType> {
    let mut locals = BTreeMap::new();
    for block in blocks {
        for instruction in &block.instructions {
            if let IRInstruction::LocalDecl { local, ty } = instruction
                && locals.insert(*local, ty.clone()).is_some()
            {
                seal_panic(&format!("{owner} declares local `{local}` more than once"));
            }
        }
    }
    for param in params {
        let Some(declared) = locals.get(&param.local_id) else {
            continue;
        };
        require_same_type(
            declared,
            &param.ty,
            &format!("{owner} parameter slot `{}`", param.local_id),
        );
    }
    locals
}

fn collect_value_types(
    blocks: &[IRBasicBlock],
    params: &[IRFunctionParam],
    owner: &str,
    environment: &TypeEnvironment<'_>,
) -> BTreeMap<ValueId, IRType> {
    let mut values = BTreeMap::new();
    for param in params {
        insert_value_type(&mut values, param.id, param.ty.clone(), owner);
    }
    for block in blocks {
        for param in &block.params {
            insert_value_type(&mut values, param.dest, param.ty.clone(), owner);
        }
        for instruction in &block.instructions {
            if let Some((dest, ty)) = instruction_result_type(instruction, environment) {
                insert_value_type(&mut values, dest, ty, owner);
            }
        }
    }
    values
}

fn insert_value_type(
    values: &mut BTreeMap<ValueId, IRType>,
    value: ValueId,
    ty: IRType,
    owner: &str,
) {
    if values.insert(value, ty).is_some() {
        seal_panic(&format!("{owner} defines value `{value}` more than once"));
    }
}

fn instruction_result_type(
    instruction: &IRInstruction,
    environment: &TypeEnvironment<'_>,
) -> Option<(ValueId, IRType)> {
    let result = match instruction {
        IRInstruction::BinaryConstruct { dest, layout, .. } => (
            *dest,
            if layout.byte_aligned {
                IRType::Binary
            } else {
                IRType::Bits
            },
        ),
        IRInstruction::BinaryMatch { dest, .. } => (*dest, IRType::Bool),
        IRInstruction::BinaryOp {
            dest,
            op,
            operand_ty,
            ..
        } => (
            *dest,
            if matches!(
                op,
                IRBinOp::Eq
                    | IRBinOp::Gt
                    | IRBinOp::GtEq
                    | IRBinOp::Lt
                    | IRBinOp::LtEq
                    | IRBinOp::NotEq
            ) {
                IRType::Bool
            } else {
                operand_ty.clone()
            },
        ),
        IRInstruction::Call { callee, dest, .. } => {
            (*dest, environment.function(callee).return_type.clone())
        }
        IRInstruction::CallClosure {
            dest, result_ty, ..
        } => (*dest, result_ty.clone()),
        IRInstruction::Clone { dest, ty, .. } => (*dest, ty.clone()),
        IRInstruction::Concat { dest, kind, .. } => (*dest, kind.ir_type()),
        IRInstruction::Const { dest, value } => (*dest, const_type(value)),
        IRInstruction::DeepCopy { dest, ty, .. } => (*dest, ty.clone()),
        IRInstruction::DropLocal { .. } | IRInstruction::DropValue { .. } => return None,
        IRInstruction::EnumConstruct { dest, ty, .. } => (*dest, IRType::Enum(ty.clone())),
        IRInstruction::EnumPayloadFieldGet {
            dest, field_type, ..
        } => (*dest, field_type.clone()),
        IRInstruction::EnumTagGet { dest, .. } => (*dest, IRType::Int8),
        IRInstruction::FieldGet {
            dest, field_type, ..
        } => (*dest, field_type.clone()),
        IRInstruction::FieldSet {
            dest,
            struct_symbol,
            ..
        } => (*dest, IRType::Struct(struct_symbol.clone())),
        IRInstruction::LoadCapture { dest, ty, .. } | IRInstruction::LoadConst { dest, ty, .. } => {
            (*dest, ty.clone())
        }
        IRInstruction::LocalDecl { .. } => return None,
        IRInstruction::LocalRead { dest, ty, .. } => (*dest, ty.clone()),
        IRInstruction::LocalWrite { .. } => return None,
        IRInstruction::MakeClosure { dest, ty, .. } => (*dest, ty.clone()),
        IRInstruction::NumericWiden { dest, to, .. } => (*dest, to.clone()),
        IRInstruction::ProcessExit { .. } => return None,
        IRInstruction::Receive {
            dest, result_type, ..
        } => (*dest, result_type.clone()),
        IRInstruction::SetPriority { .. } => return None,
        IRInstruction::Spawn { dest, ref_type, .. } => (*dest, IRType::Struct(ref_type.clone())),
        IRInstruction::StructInit { dest, ty, .. } => (*dest, IRType::Struct(ty.clone())),
        IRInstruction::TupleGet {
            dest, element_type, ..
        } => (*dest, element_type.clone()),
        IRInstruction::TupleInit { dest, ty, .. } => (*dest, IRType::Tuple(ty.clone())),
        IRInstruction::UnaryOp {
            dest,
            op,
            operand_ty,
            ..
        } => (
            *dest,
            if matches!(op, IRUnaryOp::Not) {
                IRType::Bool
            } else {
                operand_ty.clone()
            },
        ),
        IRInstruction::UnionPayloadGet {
            dest, member_type, ..
        } => (*dest, member_type.clone()),
        IRInstruction::UnionTagGet { dest, .. } => (*dest, IRType::Int8),
        IRInstruction::UnionWrap { dest, ty, .. } => (*dest, ty.clone()),
        IRInstruction::YieldCheck => return None,
    };
    Some(result)
}

fn seal_instruction_types(
    instruction: &IRInstruction,
    values: &BTreeMap<ValueId, IRType>,
    locals: &BTreeMap<IRLocalId, IRType>,
    owner: &str,
    environment: &TypeEnvironment<'_>,
    function: Option<&IRFunction>,
) {
    match instruction {
        IRInstruction::BinaryConstruct { segments, .. } => {
            for segment in segments {
                let (value, valid) = match segment {
                    LoweredBinarySegment::Float { value, .. } => {
                        (*value, value_type(values, *value, owner).is_float())
                    }
                    LoweredBinarySegment::Integer { value, .. } => {
                        (*value, value_type(values, *value, owner).is_int())
                    }
                    LoweredBinarySegment::String { value, .. } => {
                        (*value, value_type(values, *value, owner) == &IRType::String)
                    }
                };
                if !valid {
                    seal_panic(&format!(
                        "{owner} binary segment value `{value}` has incompatible type `{:?}`",
                        value_type(values, value, owner)
                    ));
                }
            }
        }
        IRInstruction::BinaryMatch {
            segments, subject, ..
        } => {
            require_one_of(
                value_type(values, *subject, owner),
                &[IRType::Binary, IRType::Bits],
                &format!("{owner} BinaryMatch subject `{subject}`"),
            );
            seal_binary_pattern_locals(segments, locals, owner);
        }
        IRInstruction::BinaryOp {
            lhs,
            operand_ty,
            rhs,
            ..
        } => {
            require_value_type(values, *lhs, operand_ty, owner, "BinaryOp lhs");
            require_value_type(values, *rhs, operand_ty, owner, "BinaryOp rhs");
        }
        IRInstruction::Call { args, callee, .. } => {
            let target = environment.function(callee);
            require_arguments(args, &target.params, values, owner, "Call");
        }
        IRInstruction::CallClosure {
            args,
            callee,
            result_ty,
            ..
        } => {
            let IRType::Function { params, ret } = value_type(values, *callee, owner) else {
                seal_panic(&format!(
                    "{owner} CallClosure callee `{callee}` is not a function"
                ));
            };
            require_argument_types(args, params, values, owner, "CallClosure");
            require_same_type(ret, result_ty, &format!("{owner} CallClosure result type"));
        }
        IRInstruction::Clone { source, ty, .. } => {
            require_value_type(values, *source, ty, owner, "copy source");
        }
        IRInstruction::Concat { kind, lhs, rhs, .. } => {
            let expected = kind.ir_type();
            require_value_type(values, *lhs, &expected, owner, "Concat lhs");
            require_value_type(values, *rhs, &expected, owner, "Concat rhs");
        }
        IRInstruction::Const { .. } => {}
        IRInstruction::DeepCopy { source, ty, .. } => {
            require_value_type(values, *source, ty, owner, "copy source");
        }
        IRInstruction::DropLocal { local, ty } => {
            require_local_type(locals, *local, ty, owner, "DropLocal");
        }
        IRInstruction::DropValue { value, ty } => {
            require_value_type(values, *value, ty, owner, "DropValue");
        }
        IRInstruction::EnumConstruct {
            payload, tag, ty, ..
        } => {
            let declaration = environment.enum_decl(ty);
            // Out-of-range tags panic in `super::enums::seal_enum_ops`;
            // this pass only types the payload values.
            let Some(variant) = declaration.variants.get(tag.0 as usize) else {
                return;
            };
            seal_enum_payload_types(payload, &variant.payload, values, owner);
        }
        IRInstruction::EnumPayloadFieldGet { value, ty, .. }
        | IRInstruction::EnumTagGet { value, ty, .. } => {
            require_value_type(
                values,
                *value,
                &IRType::Enum(ty.clone()),
                owner,
                "enum projection",
            );
        }
        IRInstruction::FieldGet {
            base,
            struct_symbol,
            ..
        } => {
            require_value_type(
                values,
                *base,
                &IRType::Struct(struct_symbol.clone()),
                owner,
                "FieldGet base",
            );
        }
        IRInstruction::FieldSet {
            base,
            field_type,
            struct_symbol,
            value,
            ..
        } => {
            require_value_type(
                values,
                *base,
                &IRType::Struct(struct_symbol.clone()),
                owner,
                "FieldSet base",
            );
            require_value_type(values, *value, field_type, owner, "FieldSet value");
        }
        IRInstruction::LoadCapture {
            capture_index, ty, ..
        } => {
            // Stray `LoadCapture` in non-closure functions and
            // out-of-range capture indexes panic in
            // `super::closures::seal_closure_ops`; this pass only
            // types the in-range slot.
            let Some(function) = function else {
                return;
            };
            let (FunctionKind::Closure { env_layout }
            | FunctionKind::DropClosureGlue { env_layout }) = &function.kind
            else {
                return;
            };
            if let Some(expected) = env_layout.get(*capture_index as usize) {
                require_same_type(ty, expected, &format!("{owner} LoadCapture type"));
            }
        }
        IRInstruction::LoadConst { const_id, ty, .. } => {
            let expected = constant_type(environment.constant(const_id));
            require_same_type(ty, &expected, &format!("{owner} LoadConst type"));
        }
        IRInstruction::LocalDecl { .. } => {}
        IRInstruction::LocalRead { local, ty, .. } => {
            require_local_type(locals, *local, ty, owner, "LocalRead");
        }
        IRInstruction::LocalWrite { local, value } => {
            let expected = local_type(locals, *local, owner);
            require_value_type(values, *value, expected, owner, "LocalWrite");
        }
        IRInstruction::MakeClosure {
            body, captures, ty, ..
        } => {
            let target = environment.function(body);
            // A non-closure body kind panics in
            // `super::closures::seal_closure_ops`.
            let FunctionKind::Closure { env_layout } = &target.kind else {
                return;
            };
            require_argument_types(captures, env_layout, values, owner, "MakeClosure");
            let expected = IRType::Function {
                params: target.params.iter().map(|param| param.ty.clone()).collect(),
                ret: Box::new(target.return_type.clone()),
            };
            require_same_type(ty, &expected, &format!("{owner} MakeClosure type"));
        }
        IRInstruction::NumericWiden {
            from, to, value, ..
        } => {
            require_value_type(values, *value, from, owner, "NumericWiden source");
            if !(from.is_int() && to.is_int() || from.is_float() && to.is_float()) {
                seal_panic(&format!(
                    "{owner} NumericWiden cannot widen `{from:?}` to `{to:?}`"
                ));
            }
        }
        IRInstruction::ProcessExit { reason } => {
            require_value_type(values, *reason, &IRType::Int64, owner, "ProcessExit reason");
        }
        IRInstruction::Receive { after, arms, .. } => {
            if let Some(after) = after {
                require_value_type(
                    values,
                    after.timeout,
                    &IRType::Int64,
                    owner,
                    "Receive timeout",
                );
            }
            for arm in arms {
                require_local_type(
                    locals,
                    arm.payload_local,
                    &arm.payload_type,
                    owner,
                    "Receive payload",
                );
            }
        }
        IRInstruction::SetPriority { tag } => {
            require_value_type(values, *tag, &IRType::Int64, owner, "SetPriority tag");
        }
        IRInstruction::Spawn {
            config,
            config_type,
            ..
        } => {
            require_value_type(values, *config, config_type, owner, "Spawn config");
        }
        IRInstruction::StructInit { fields, ty, .. } => {
            let declaration = environment.struct_decl(ty);
            // Arity and index-order violations panic in
            // `super::structs::seal_struct_ops`; this pass only types
            // the in-range field values.
            for field in fields {
                if let Some(expected) = declaration.fields.get(field.index as usize) {
                    require_value_type(
                        values,
                        field.value,
                        unboxed_type(&expected.ir_type),
                        owner,
                        "StructInit field",
                    );
                }
            }
        }
        IRInstruction::TupleGet {
            base,
            element_type,
            index,
            ..
        } => {
            let IRType::Tuple(elements) = value_type(values, *base, owner) else {
                seal_panic(&format!("{owner}: TupleGet base `{base}` is not a tuple",));
            };
            let Some(declared) = elements.get(*index as usize) else {
                seal_panic(&format!(
                    "{owner}: TupleGet references element index {index}, but the tuple \
                     only has {count} element(s)",
                    count = elements.len(),
                ));
            };
            require_same_type(
                element_type,
                declared,
                &format!("{owner} TupleGet element `{index}`"),
            );
        }
        IRInstruction::TupleInit { elements, ty, .. } => {
            if elements.len() != ty.len() {
                seal_panic(&format!(
                    "{owner}: TupleInit carries {got} element(s) but its type has {expected}",
                    got = elements.len(),
                    expected = ty.len(),
                ));
            }
            for (element, expected) in elements.iter().zip(ty) {
                require_value_type(values, *element, expected, owner, "TupleInit element");
            }
        }
        IRInstruction::UnaryOp {
            op,
            operand,
            operand_ty,
            ..
        } => {
            require_value_type(values, *operand, operand_ty, owner, "UnaryOp operand");
            if matches!(op, IRUnaryOp::Not) {
                require_same_type(
                    operand_ty,
                    &IRType::Bool,
                    &format!("{owner} UnaryOp Not type"),
                );
            }
        }
        IRInstruction::UnionPayloadGet {
            member_index,
            member_type,
            ty,
            value,
            ..
        } => {
            seal_union_projection(
                *member_index,
                member_type,
                ty,
                *value,
                values,
                owner,
                environment,
            );
        }
        IRInstruction::UnionTagGet { ty, value, .. } => {
            require_value_type(values, *value, ty, owner, "UnionTagGet value");
        }
        IRInstruction::UnionWrap {
            member_index,
            member_type,
            ty,
            value,
            ..
        } => {
            require_value_type(values, *value, member_type, owner, "UnionWrap value");
            let IRType::Union { mangled, members } = ty else {
                seal_panic(&format!("{owner} UnionWrap target is not a union"));
            };
            let declaration = environment.union_decl(mangled);
            require_same_type(
                &IRType::Union {
                    mangled: declaration.symbol.clone(),
                    members: declaration.members.clone(),
                },
                ty,
                &format!("{owner} UnionWrap declaration"),
            );
            let Some(expected) = members.get(*member_index as usize) else {
                seal_panic(&format!(
                    "{owner} UnionWrap member index {member_index} is out of range"
                ));
            };
            require_same_type(member_type, expected, &format!("{owner} UnionWrap member"));
        }
        IRInstruction::YieldCheck => {}
    }
}

fn seal_terminator_types(
    terminator: &IRTerminator,
    blocks: &[IRBasicBlock],
    values: &BTreeMap<ValueId, IRType>,
    params: &[IRFunctionParam],
    owner: &str,
) {
    match terminator {
        IRTerminator::Branch(target) => {
            seal_branch_types(target.block, &target.args, blocks, values, owner);
        }
        IRTerminator::CondBranch {
            cond,
            else_target,
            then_target,
        } => {
            require_value_type(values, *cond, &IRType::Bool, owner, "CondBranch condition");
            seal_branch_types(then_target.block, &then_target.args, blocks, values, owner);
            seal_branch_types(else_target.block, &else_target.args, blocks, values, owner);
        }
        // Deliberately unchecked: typecheck only validates trailing
        // expressions today, so an explicit `return`'s value carries
        // no type guarantee the seal could hold lowering to. Validate
        // against the declared return type once typecheck checks
        // explicit returns.
        IRTerminator::Return { .. } => {}
        IRTerminator::TailCall { args, .. } => {
            require_arguments(args, params, values, owner, "TailCall");
        }
        IRTerminator::Unreachable => {}
    }
}

fn seal_branch_types(
    target: IRBlockId,
    args: &[ValueId],
    blocks: &[IRBasicBlock],
    values: &BTreeMap<ValueId, IRType>,
    owner: &str,
) {
    let Some(block) = blocks.iter().find(|block| block.id == target) else {
        return;
    };
    let expected: Vec<IRType> = block.params.iter().map(|param| param.ty.clone()).collect();
    require_argument_types(args, &expected, values, owner, "Branch");
}

fn seal_binary_pattern_locals(
    patterns: &[LoweredBinaryPattern],
    locals: &BTreeMap<IRLocalId, IRType>,
    owner: &str,
) {
    for pattern in patterns {
        match pattern {
            LoweredBinaryPattern::BindInt { local, ty, .. } => {
                require_local_type(locals, *local, ty, owner, "BinaryMatch binding");
            }
            LoweredBinaryPattern::GreedyTail {
                local: Some(local),
                ty,
                ..
            } => {
                require_local_type(locals, *local, ty, owner, "BinaryMatch tail");
            }
            LoweredBinaryPattern::Discard { .. }
            | LoweredBinaryPattern::GreedyTail { local: None, .. }
            | LoweredBinaryPattern::LiteralBytes { .. }
            | LoweredBinaryPattern::LiteralInt { .. } => {}
        }
    }
}

fn seal_enum_payload_types(
    actual: &EnumPayloadInit,
    expected: &IRVariantPayload,
    values: &BTreeMap<ValueId, IRType>,
    owner: &str,
) {
    match (actual, expected) {
        (EnumPayloadInit::Struct(actual), IRVariantPayload::Struct(expected)) => {
            for field in actual {
                if let Some(expected) = expected.get(field.index as usize) {
                    require_value_type(
                        values,
                        field.value,
                        unboxed_type(&expected.ir_type),
                        owner,
                        "EnumConstruct field",
                    );
                }
            }
        }
        (EnumPayloadInit::Tuple(actual), IRVariantPayload::Tuple(expected)) => {
            require_unboxed_argument_types(
                actual,
                expected,
                values,
                owner,
                "EnumConstruct payload",
            );
        }
        (EnumPayloadInit::Unit, IRVariantPayload::Unit) => {}
        // Payload shape mismatches panic in
        // `super::enums::seal_enum_ops`.
        _ => {}
    }
}

fn seal_union_projection(
    member_index: u8,
    member_type: &IRType,
    ty: &IRType,
    value: ValueId,
    values: &BTreeMap<ValueId, IRType>,
    owner: &str,
    environment: &TypeEnvironment<'_>,
) {
    require_value_type(values, value, ty, owner, "UnionPayloadGet value");
    let IRType::Union { mangled, members } = ty else {
        seal_panic(&format!("{owner} UnionPayloadGet source is not a union"));
    };
    environment.union_decl(mangled);
    let Some(expected) = members.get(member_index as usize) else {
        seal_panic(&format!(
            "{owner} UnionPayloadGet member index {member_index} is out of range"
        ));
    };
    require_same_type(
        member_type,
        expected,
        &format!("{owner} UnionPayloadGet member"),
    );
}

fn require_arguments(
    args: &[ValueId],
    params: &[IRFunctionParam],
    values: &BTreeMap<ValueId, IRType>,
    owner: &str,
    operation: &str,
) {
    let expected: Vec<IRType> = params.iter().map(|param| param.ty.clone()).collect();
    require_argument_types(args, &expected, values, owner, operation);
}

fn require_argument_types(
    args: &[ValueId],
    expected: &[IRType],
    values: &BTreeMap<ValueId, IRType>,
    owner: &str,
    operation: &str,
) {
    require_argument_types_with(args, expected, values, owner, operation, false);
}

fn require_argument_types_with(
    args: &[ValueId],
    expected: &[IRType],
    values: &BTreeMap<ValueId, IRType>,
    owner: &str,
    operation: &str,
    unbox_indirect: bool,
) {
    if args.len() != expected.len() {
        seal_panic(&format!(
            "{owner} {operation} passes {} argument(s), expected {}",
            args.len(),
            expected.len()
        ));
    }
    for (index, (arg, expected)) in args.iter().zip(expected).enumerate() {
        let expected = if unbox_indirect {
            unboxed_type(expected)
        } else {
            expected
        };
        require_value_type(
            values,
            *arg,
            expected,
            owner,
            &format!("{operation} argument #{index}"),
        );
    }
}

fn require_unboxed_argument_types(
    args: &[ValueId],
    expected: &[IRType],
    values: &BTreeMap<ValueId, IRType>,
    owner: &str,
    operation: &str,
) {
    require_argument_types_with(args, expected, values, owner, operation, true);
}

fn require_local_type(
    locals: &BTreeMap<IRLocalId, IRType>,
    local: IRLocalId,
    expected: &IRType,
    owner: &str,
    operation: &str,
) {
    let actual = local_type(locals, local, owner);
    require_same_type(
        actual,
        expected,
        &format!("{owner} {operation} local `{local}`"),
    );
}

fn require_value_type(
    values: &BTreeMap<ValueId, IRType>,
    value: ValueId,
    expected: &IRType,
    owner: &str,
    operation: &str,
) {
    let actual = value_type(values, value, owner);
    require_same_type(
        actual,
        expected,
        &format!("{owner} {operation} value `{value}`"),
    );
}

fn require_same_type(actual: &IRType, expected: &IRType, location: &str) {
    if actual != expected {
        seal_panic(&format!(
            "{location} has type `{actual:?}`, expected `{expected:?}`"
        ));
    }
}

fn require_one_of(actual: &IRType, expected: &[IRType], location: &str) {
    if !expected.contains(actual) {
        seal_panic(&format!(
            "{location} has type `{actual:?}`, expected one of `{expected:?}`"
        ));
    }
}

fn local_type<'a>(
    locals: &'a BTreeMap<IRLocalId, IRType>,
    local: IRLocalId,
    owner: &str,
) -> &'a IRType {
    locals
        .get(&local)
        .unwrap_or_else(|| seal_panic(&format!("{owner} references undeclared local `{local}`")))
}

fn value_type<'a>(
    values: &'a BTreeMap<ValueId, IRType>,
    value: ValueId,
    owner: &str,
) -> &'a IRType {
    values.get(&value).unwrap_or_else(|| {
        seal_panic(&format!(
            "{owner} references value `{value}` without a recorded type"
        ))
    })
}

fn const_type(value: &ConstValue) -> IRType {
    match value {
        ConstValue::Binary(_) => IRType::Binary,
        ConstValue::Bits { .. } => IRType::Bits,
        ConstValue::Bool(_) => IRType::Bool,
        ConstValue::Float32(_) => IRType::Float32,
        ConstValue::Float64(_) => IRType::Float64,
        ConstValue::Int8(_) => IRType::Int8,
        ConstValue::Int16(_) => IRType::Int16,
        ConstValue::Int32(_) => IRType::Int32,
        ConstValue::Int64(_) => IRType::Int64,
        ConstValue::String(_) => IRType::String,
        ConstValue::UInt8(_) => IRType::UInt8,
        ConstValue::UInt16(_) => IRType::UInt16,
        ConstValue::UInt32(_) => IRType::UInt32,
        ConstValue::UInt64(_) => IRType::UInt64,
        ConstValue::Unit => IRType::Unit,
    }
}

fn constant_type(value: &IRConstantValue) -> IRType {
    match value {
        IRConstantValue::EnumVariant { ty, .. } => IRType::Enum(ty.clone()),
        IRConstantValue::Primitive(value) => const_type(value),
        IRConstantValue::Struct { ty, .. } => IRType::Struct(ty.clone()),
    }
}

fn unboxed_type(ty: &IRType) -> &IRType {
    match ty {
        IRType::Indirect(inner) => inner,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use koja_ast::identifier::LocalId;

    use super::*;
    use crate::function::{
        BlockParam, BranchTarget, IRBasicBlock, IRBlockId, IRFunctionParam, IRTerminator,
    };

    fn block(
        id: u32,
        instructions: Vec<IRInstruction>,
        params: Vec<BlockParam>,
        terminator: IRTerminator,
    ) -> IRBasicBlock {
        IRBasicBlock {
            id: IRBlockId(id),
            instructions,
            label: format!("bb{id}"),
            params,
            terminator,
        }
    }

    fn function(
        symbol: IRSymbol,
        blocks: Vec<IRBasicBlock>,
        params: Vec<IRFunctionParam>,
        return_type: IRType,
    ) -> IRFunction {
        IRFunction {
            blocks,
            def_location: None,
            kind: FunctionKind::Regular,
            params,
            return_type,
            symbol,
        }
    }

    fn local(id: u32) -> IRLocalId {
        IRLocalId::from_local_id(LocalId::new(id))
    }

    fn package(functions: Vec<IRFunction>) -> IRPackage {
        IRPackage {
            constants: BTreeMap::new(),
            enums: BTreeMap::new(),
            functions: functions
                .into_iter()
                .map(|function| (function.symbol.clone(), function))
                .collect(),
            package: "Test".to_string(),
            structs: BTreeMap::new(),
            unions: BTreeMap::new(),
        }
    }

    fn symbol(name: &str) -> IRSymbol {
        IRSymbol::synthetic(format!("Test.{name}"))
    }

    #[test]
    #[should_panic(expected = "Branch argument #0")]
    fn branch_argument_type_mismatch_panics() {
        let function = function(
            symbol("branch"),
            vec![
                block(
                    0,
                    vec![IRInstruction::Const {
                        dest: ValueId(0),
                        value: ConstValue::Bool(true),
                    }],
                    Vec::new(),
                    IRTerminator::Branch(BranchTarget::with_args(IRBlockId(1), vec![ValueId(0)])),
                ),
                block(
                    1,
                    Vec::new(),
                    vec![BlockParam {
                        dest: ValueId(1),
                        ty: IRType::Int64,
                    }],
                    IRTerminator::Return {
                        value: Some(ValueId(1)),
                    },
                ),
            ],
            Vec::new(),
            IRType::Int64,
        );
        let package = package(vec![function]);
        let environment = TypeEnvironment::new(std::slice::from_ref(&package));
        seal_package_types(&package, &environment);
    }

    #[test]
    #[should_panic(expected = "Call argument #0")]
    fn call_argument_type_mismatch_panics() {
        let callee_symbol = symbol("callee");
        let callee = function(
            callee_symbol.clone(),
            Vec::new(),
            vec![IRFunctionParam {
                id: ValueId(0),
                local_id: local(0),
                ty: IRType::Int64,
            }],
            IRType::Unit,
        );
        let caller = function(
            symbol("caller"),
            vec![block(
                0,
                vec![
                    IRInstruction::Const {
                        dest: ValueId(0),
                        value: ConstValue::Bool(true),
                    },
                    IRInstruction::Call {
                        args: vec![ValueId(0)],
                        callee: callee_symbol,
                        dest: ValueId(1),
                    },
                ],
                Vec::new(),
                IRTerminator::Return { value: None },
            )],
            Vec::new(),
            IRType::Unit,
        );
        let package = package(vec![callee, caller]);
        let environment = TypeEnvironment::new(std::slice::from_ref(&package));
        seal_package_types(&package, &environment);
    }

    #[test]
    #[should_panic(expected = "LocalRead local")]
    fn local_read_type_mismatch_panics() {
        let local = local(0);
        let function = function(
            symbol("local"),
            vec![block(
                0,
                vec![
                    IRInstruction::LocalDecl {
                        local,
                        ty: IRType::Int64,
                    },
                    IRInstruction::LocalRead {
                        dest: ValueId(0),
                        local,
                        ty: IRType::Bool,
                    },
                ],
                Vec::new(),
                IRTerminator::Return { value: None },
            )],
            Vec::new(),
            IRType::Unit,
        );
        let package = package(vec![function]);
        let environment = TypeEnvironment::new(std::slice::from_ref(&package));
        seal_package_types(&package, &environment);
    }

    #[test]
    #[should_panic(expected = "TailCall argument #0")]
    fn tail_call_argument_type_mismatch_panics() {
        let symbol = symbol("tail");
        let function = function(
            symbol.clone(),
            vec![block(
                0,
                vec![
                    IRInstruction::LocalDecl {
                        local: local(0),
                        ty: IRType::Int64,
                    },
                    IRInstruction::Const {
                        dest: ValueId(1),
                        value: ConstValue::Bool(true),
                    },
                ],
                Vec::new(),
                IRTerminator::TailCall {
                    args: vec![ValueId(1)],
                    callee: symbol,
                },
            )],
            vec![IRFunctionParam {
                id: ValueId(0),
                local_id: local(0),
                ty: IRType::Int64,
            }],
            IRType::Unit,
        );
        let package = package(vec![function]);
        let environment = TypeEnvironment::new(std::slice::from_ref(&package));
        seal_package_types(&package, &environment);
    }
}
