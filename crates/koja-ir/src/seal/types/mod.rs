//! Typed IR invariants shared by program and script sealing.
//!
//! The structural seal proves that every value exists. This pass also
//! proves that each use agrees with the producer's concrete IR type.

mod binary;
mod closures;
mod enums;
mod process;
mod structs;
mod tuples;
mod unions;

use std::collections::BTreeMap;

use crate::constant::IRConstantValue;
use crate::declarations::Declarations;
use crate::function::{
    IRBasicBlock, IRBlockId, IRFunction, IRFunctionParam, IRInstruction, IRSymbol, IRTerminator,
};
use crate::local::IRLocalId;
use crate::package::IRPackage;
use crate::types::{IRBinOp, IRType, IRUnaryOp, ValueId};

use super::seal_panic;

/// The declaration `found` names, or a seal panic that names `kind`
/// and `symbol` when no package registers it.
fn registered<'a, T>(found: Option<&'a T>, kind: &str, symbol: &IRSymbol) -> &'a T {
    found.unwrap_or_else(|| {
        seal_panic(&format!(
            "{kind} `{symbol}` is not registered in any package"
        ))
    })
}

/// Type-check every function body in `package`. Bodiless kinds
/// (externs, intrinsics, backend-synthesized glue) are skipped.
pub(super) fn seal_package_types(package: &IRPackage, declarations: &Declarations<'_>) {
    for function in package.functions.values() {
        seal_function_types(function, declarations);
    }
}

/// The body a `Return` terminator exits. Function bodies hold bare
/// returns to `Unit`. Script bodies allow them anywhere, since a
/// top-level `return` exits the script early and discards the
/// trailing value.
enum ReturnSite<'a> {
    Function { return_type: &'a IRType },
    ScriptBody { return_type: &'a IRType },
}

impl ReturnSite<'_> {
    fn return_type(&self) -> &IRType {
        match self {
            ReturnSite::Function { return_type } | ReturnSite::ScriptBody { return_type } => {
                return_type
            }
        }
    }
}

/// Type-check a script body, which has no params and allows a
/// `Return` anywhere.
pub(super) fn seal_script_body_types(
    blocks: &[IRBasicBlock],
    return_type: &IRType,
    declarations: &Declarations<'_>,
) {
    let site = ReturnSite::ScriptBody { return_type };
    seal_body_types(blocks, &[], "script body", declarations, None, &site);
}

fn seal_function_types(function: &IRFunction, declarations: &Declarations<'_>) {
    if function.blocks.is_empty() {
        return;
    }
    let owner = format!("function `{}`", function.symbol);
    let site = ReturnSite::Function {
        return_type: &function.return_type,
    };
    seal_body_types(
        &function.blocks,
        &function.params,
        &owner,
        declarations,
        Some(function),
        &site,
    );
}

/// Two passes over a body. The first collects every local and value
/// type, the second checks each use against them, so use order
/// within the body does not matter.
fn seal_body_types(
    blocks: &[IRBasicBlock],
    params: &[IRFunctionParam],
    owner: &str,
    declarations: &Declarations<'_>,
    function: Option<&IRFunction>,
    site: &ReturnSite<'_>,
) {
    let locals = collect_local_types(blocks, params, owner);
    let values = collect_value_types(blocks, params, owner, declarations);
    let scope = TypeScope {
        declarations,
        function,
        locals: &locals,
        owner,
        values: &values,
    };

    for block in blocks {
        for instruction in &block.instructions {
            seal_instruction_types(instruction, &scope);
        }
        seal_terminator_types(&block.terminator, blocks, params, &scope, site);
    }
}

/// Everything one body's checks read. The walker builds one scope
/// per body and each check takes it by reference.
struct TypeScope<'a> {
    declarations: &'a Declarations<'a>,
    /// The enclosing function, or `None` for a script body. Closure
    /// checks read its kind and params.
    function: Option<&'a IRFunction>,
    locals: &'a BTreeMap<IRLocalId, IRType>,
    /// Names the body in panic messages.
    owner: &'a str,
    values: &'a BTreeMap<ValueId, IRType>,
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
    // A param slot only exists when the body promoted the param, so
    // a missing decl is not an error.
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
    declarations: &Declarations<'_>,
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
            if let Some((dest, ty)) = instruction_result_type(instruction, declarations) {
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

/// The `(dest, type)` an instruction defines, or `None` for
/// instructions with no result.
fn instruction_result_type(
    instruction: &IRInstruction,
    declarations: &Declarations<'_>,
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
        IRInstruction::Call { callee, dest, .. } => (
            *dest,
            registered(declarations.function(callee), "function", callee)
                .return_type
                .clone(),
        ),
        IRInstruction::CallClosure {
            dest, result_ty, ..
        } => (*dest, result_ty.clone()),
        IRInstruction::Clone { dest, ty, .. } => (*dest, ty.clone()),
        IRInstruction::ClosureEquals { dest, .. } => (*dest, IRType::Bool),
        IRInstruction::Concat { dest, kind, .. } => (*dest, kind.ir_type()),
        IRInstruction::Const { dest, value } => (*dest, value.ir_type()),
        IRInstruction::DeepCopy { dest, ty, .. } => (*dest, ty.clone()),
        IRInstruction::ConsumeLocal { .. }
        | IRInstruction::DropLocal { .. }
        | IRInstruction::DropValue { .. } => return None,
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
        IRInstruction::IndirectPresent { dest, .. } => (*dest, IRType::Bool),
        IRInstruction::LoadCapture { dest, ty, .. }
        | IRInstruction::LoadCaptureOf { dest, ty, .. }
        | IRInstruction::LoadConst { dest, ty, .. } => (*dest, ty.clone()),
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

/// Check one instruction's operands against the scope. Arms that fit
/// on a few lines stay here. The rest delegate to their family
/// module.
fn seal_instruction_types(instruction: &IRInstruction, scope: &TypeScope<'_>) {
    match instruction {
        IRInstruction::BinaryConstruct { segments, .. } => {
            binary::seal_binary_construct(segments, scope);
        }
        IRInstruction::BinaryMatch {
            segments, subject, ..
        } => {
            binary::seal_binary_match(segments, *subject, scope);
        }
        IRInstruction::BinaryOp {
            lhs,
            operand_ty,
            rhs,
            ..
        } => {
            scope.require_value_type(*lhs, operand_ty, "BinaryOp lhs");
            scope.require_value_type(*rhs, operand_ty, "BinaryOp rhs");
        }
        IRInstruction::Call { args, callee, .. } => {
            let target = registered(scope.declarations.function(callee), "function", callee);
            scope.require_arguments(args, &target.params, "Call");
        }
        IRInstruction::CallClosure {
            args,
            callee,
            param_types,
            result_ty,
            ..
        } => {
            closures::seal_call_closure(args, *callee, param_types, result_ty, scope);
        }
        IRInstruction::Clone { source, ty, .. } => {
            scope.require_value_type(*source, ty, "copy source");
        }
        IRInstruction::ClosureEquals { lhs, rhs, ty, .. } => {
            closures::seal_closure_equals(*lhs, *rhs, ty, scope);
        }
        IRInstruction::Concat { kind, lhs, rhs, .. } => {
            let expected = kind.ir_type();
            scope.require_value_type(*lhs, &expected, "Concat lhs");
            scope.require_value_type(*rhs, &expected, "Concat rhs");
        }
        IRInstruction::Const { .. } | IRInstruction::ConsumeLocal { .. } => {}
        IRInstruction::DeepCopy { source, ty, .. } => {
            scope.require_value_type(*source, ty, "copy source");
        }
        IRInstruction::DropLocal { local, ty } => {
            scope.require_local_type(*local, ty, "DropLocal");
        }
        IRInstruction::DropValue { value, ty } => {
            scope.require_value_type(*value, ty, "DropValue");
        }
        IRInstruction::EnumConstruct {
            payload, tag, ty, ..
        } => {
            enums::seal_enum_construct(payload, *tag, ty, scope);
        }
        IRInstruction::EnumPayloadFieldGet { value, ty, .. }
        | IRInstruction::EnumTagGet { value, ty, .. } => {
            enums::seal_enum_projection(*value, ty, scope);
        }
        IRInstruction::FieldGet {
            base,
            struct_symbol,
            ..
        } => {
            structs::seal_field_get(*base, struct_symbol, scope);
        }
        IRInstruction::FieldSet {
            base,
            field_type,
            struct_symbol,
            value,
            ..
        } => {
            structs::seal_field_set(*base, field_type, struct_symbol, *value, scope);
        }
        IRInstruction::IndirectPresent { base, slot, .. } => {
            structs::seal_indirect_present(*base, slot, scope);
        }
        IRInstruction::LoadCapture {
            capture_index, ty, ..
        } => {
            closures::seal_load_capture(*capture_index, ty, scope);
        }
        IRInstruction::LoadCaptureOf {
            capture_index,
            closure,
            ty,
            ..
        } => {
            closures::seal_load_capture_of(*capture_index, *closure, ty, scope);
        }
        IRInstruction::LoadConst { const_id, ty, .. } => {
            let expected = constant_type(registered(
                scope.declarations.constant_value(const_id),
                "constant",
                const_id,
            ));
            require_same_type(ty, &expected, &format!("{} LoadConst type", scope.owner));
        }
        IRInstruction::LocalDecl { .. } => {}
        IRInstruction::LocalRead { local, ty, .. } => {
            scope.require_local_type(*local, ty, "LocalRead");
        }
        IRInstruction::LocalWrite { local, value } => {
            let expected = scope.local_type(*local);
            scope.require_value_type(*value, expected, "LocalWrite");
        }
        IRInstruction::MakeClosure {
            body, captures, ty, ..
        } => {
            closures::seal_make_closure(body, captures, ty, scope);
        }
        IRInstruction::NumericWiden {
            from, to, value, ..
        } => {
            scope.require_value_type(*value, from, "NumericWiden source");
            if !(from.is_int() && to.is_int() || from.is_float() && to.is_float()) {
                seal_panic(&format!(
                    "{} NumericWiden cannot widen `{from:?}` to `{to:?}`",
                    scope.owner
                ));
            }
        }
        IRInstruction::ProcessExit { reason } => {
            scope.require_value_type(*reason, &IRType::Int64, "ProcessExit reason");
        }
        IRInstruction::Receive { after, arms, .. } => {
            process::seal_receive(after.as_ref(), arms, scope);
        }
        IRInstruction::SetPriority { tag } => {
            scope.require_value_type(*tag, &IRType::Int64, "SetPriority tag");
        }
        IRInstruction::Spawn {
            config,
            config_type,
            ..
        } => {
            process::seal_spawn(*config, config_type, scope);
        }
        IRInstruction::StructInit { fields, ty, .. } => {
            structs::seal_struct_init(fields, ty, scope);
        }
        IRInstruction::TupleGet {
            base,
            element_type,
            index,
            ..
        } => {
            tuples::seal_tuple_get(*base, element_type, *index, scope);
        }
        IRInstruction::TupleInit { elements, ty, .. } => {
            tuples::seal_tuple_init(elements, ty, scope);
        }
        IRInstruction::UnaryOp {
            op,
            operand,
            operand_ty,
            ..
        } => {
            scope.require_value_type(*operand, operand_ty, "UnaryOp operand");
            if matches!(op, IRUnaryOp::Not) {
                require_same_type(
                    operand_ty,
                    &IRType::Bool,
                    &format!("{} UnaryOp Not type", scope.owner),
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
            unions::seal_union_payload_get(*member_index, member_type, ty, *value, scope);
        }
        IRInstruction::UnionTagGet { ty, value, .. } => {
            unions::seal_union_tag_get(ty, *value, scope);
        }
        IRInstruction::UnionWrap {
            member_index,
            member_type,
            ty,
            value,
            ..
        } => {
            unions::seal_union_wrap(*member_index, member_type, ty, *value, scope);
        }
        IRInstruction::YieldCheck => {}
    }
}

fn seal_terminator_types(
    terminator: &IRTerminator,
    blocks: &[IRBasicBlock],
    params: &[IRFunctionParam],
    scope: &TypeScope<'_>,
    site: &ReturnSite<'_>,
) {
    match terminator {
        IRTerminator::Branch(target) => {
            seal_branch_types(target.block, &target.args, blocks, scope);
        }
        IRTerminator::CondBranch {
            cond,
            else_target,
            then_target,
        } => {
            scope.require_value_type(*cond, &IRType::Bool, "CondBranch condition");
            seal_branch_types(then_target.block, &then_target.args, blocks, scope);
            seal_branch_types(else_target.block, &else_target.args, blocks, scope);
        }
        IRTerminator::Return { value } => match (value, site) {
            (Some(value), _) => {
                scope.require_value_type(*value, site.return_type(), "Return");
            }
            (None, ReturnSite::ScriptBody { .. }) => {}
            (None, ReturnSite::Function { return_type }) => require_same_type(
                &IRType::Unit,
                return_type,
                &format!("{} bare Return", scope.owner),
            ),
        },
        IRTerminator::TailCall { args, .. } => {
            scope.require_arguments(args, params, "TailCall");
        }
        IRTerminator::Unreachable => {}
    }
}

fn seal_branch_types(
    target: IRBlockId,
    args: &[ValueId],
    blocks: &[IRBasicBlock],
    scope: &TypeScope<'_>,
) {
    // The structural seal already rejects unknown targets, so only
    // the argument types are checked here.
    let Some(block) = blocks.iter().find(|block| block.id == target) else {
        return;
    };
    let expected: Vec<IRType> = block.params.iter().map(|param| param.ty.clone()).collect();
    scope.require_argument_types(args, &expected, "Branch");
}

impl TypeScope<'_> {
    /// The declared type of `local`, or a seal panic when the body
    /// never declares it.
    fn local_type(&self, local: IRLocalId) -> &IRType {
        self.locals.get(&local).unwrap_or_else(|| {
            seal_panic(&format!(
                "{} references undeclared local `{local}`",
                self.owner
            ))
        })
    }

    /// Require `args` to match `expected` in count and in type.
    fn require_argument_types(&self, args: &[ValueId], expected: &[IRType], operation: &str) {
        if args.len() != expected.len() {
            seal_panic(&format!(
                "{} {operation} passes {} argument(s), expected {}",
                self.owner,
                args.len(),
                expected.len()
            ));
        }
        for (index, (arg, expected)) in args.iter().zip(expected).enumerate() {
            self.require_value_type(*arg, expected, &format!("{operation} argument #{index}"));
        }
    }

    /// Require `args` to match the types of `params`.
    fn require_arguments(&self, args: &[ValueId], params: &[IRFunctionParam], operation: &str) {
        let expected: Vec<IRType> = params.iter().map(|param| param.ty.clone()).collect();
        self.require_argument_types(args, &expected, operation);
    }

    fn require_local_type(&self, local: IRLocalId, expected: &IRType, operation: &str) {
        require_same_type(
            self.local_type(local),
            expected,
            &format!("{} {operation} local `{local}`", self.owner),
        );
    }

    /// Require `value` to fit a declared field or payload slot. The
    /// value may carry the unboxed view, since backends box on store,
    /// or the declared `Indirect(T)` itself, since clone glue passes
    /// the shared box through.
    fn require_slot_value_type(&self, value: ValueId, declared: &IRType, operation: &str) {
        let actual = self.value_type(value);
        if actual == declared {
            return;
        }
        require_same_type(
            actual,
            declared.unboxed(),
            &format!("{} {operation} value `{value}`", self.owner),
        );
    }

    fn require_value_type(&self, value: ValueId, expected: &IRType, operation: &str) {
        require_same_type(
            self.value_type(value),
            expected,
            &format!("{} {operation} value `{value}`", self.owner),
        );
    }

    /// The recorded type of `value`, or a seal panic when nothing in
    /// the body defines it.
    fn value_type(&self, value: ValueId) -> &IRType {
        self.values.get(&value).unwrap_or_else(|| {
            seal_panic(&format!(
                "{} references value `{value}` without a recorded type",
                self.owner
            ))
        })
    }
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

fn constant_type(value: &IRConstantValue) -> IRType {
    match value {
        IRConstantValue::Built { ty, .. } => ty.clone(),
        IRConstantValue::EnumVariant { ty, .. } => IRType::Enum(ty.clone()),
        IRConstantValue::Primitive(value) => value.ir_type(),
        IRConstantValue::Struct { ty, .. } => IRType::Struct(ty.clone()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use koja_ast::identifier::LocalId;

    use super::*;
    use crate::function::{
        BlockParam, BranchTarget, FunctionKind, IRBasicBlock, IRBlockId, IRFunctionParam,
        IRTerminator,
    };
    use crate::types::ConstValue;

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
        let declarations = Declarations::new(std::slice::from_ref(&package));
        seal_package_types(&package, &declarations);
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
        let declarations = Declarations::new(std::slice::from_ref(&package));
        seal_package_types(&package, &declarations);
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
        let declarations = Declarations::new(std::slice::from_ref(&package));
        seal_package_types(&package, &declarations);
    }

    #[test]
    #[should_panic(expected = "Return value")]
    fn return_value_type_mismatch_panics() {
        let function = function(
            symbol("return_mismatch"),
            vec![block(
                0,
                vec![IRInstruction::Const {
                    dest: ValueId(0),
                    value: ConstValue::Bool(true),
                }],
                Vec::new(),
                IRTerminator::Return {
                    value: Some(ValueId(0)),
                },
            )],
            Vec::new(),
            IRType::Int64,
        );
        let package = package(vec![function]);
        let declarations = Declarations::new(std::slice::from_ref(&package));
        seal_package_types(&package, &declarations);
    }

    #[test]
    #[should_panic(expected = "bare Return")]
    fn bare_return_in_valued_function_panics() {
        let function = function(
            symbol("bare_return"),
            vec![block(
                0,
                Vec::new(),
                Vec::new(),
                IRTerminator::Return { value: None },
            )],
            Vec::new(),
            IRType::Int64,
        );
        let package = package(vec![function]);
        let declarations = Declarations::new(std::slice::from_ref(&package));
        seal_package_types(&package, &declarations);
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
        let declarations = Declarations::new(std::slice::from_ref(&package));
        seal_package_types(&package, &declarations);
    }
}
