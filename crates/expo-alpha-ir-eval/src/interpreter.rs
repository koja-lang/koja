//! Tree-walking interpreter over a sealed [`IRProgram`] / [`IRScript`].
//! Parameterized over a [`CallResolver`] so both IR shapes share the
//! per-instruction execution, frame management, and terminator
//! dispatch code; only callee lookup differs. Operator math lives in
//! [`crate::ops`].

use std::collections::BTreeMap;

use expo_alpha_ir::{
    ConstValue, EnumPayloadInit, FunctionKind, IRBasicBlock, IRBlockId, IRConstantValue,
    IREnumDecl, IRFunction, IRInstruction, IRLocalId, IRProgram, IRScript, IRSymbol, IRTerminator,
    IRVariantPayload, IRVariantTag, ValueId,
};

use crate::error::RuntimeError;
use crate::intrinsics;
use crate::ops::{apply_binary_op, apply_unary_op};
use crate::value::{EnumPayload, Value};

pub struct Interpreter;

impl Interpreter {
    /// Execute the project-mode entry function and return its result.
    pub fn run_program(program: IRProgram) -> Result<Value, RuntimeError> {
        let entry = program.entry_function();
        execute_function(entry, Vec::new(), &program)
    }

    /// Execute the script-mode implicit body and return its trailing
    /// value.
    pub fn run_script(script: IRScript) -> Result<Value, RuntimeError> {
        let mut frame = Frame::new();
        execute_blocks(&script.blocks, &mut frame, &script)
    }
}

/// Per-call execution frame. SSA values and local-slot storage live
/// in separate maps so slot identity never collides with SSA
/// identity even though both keys happen to be `u32`.
struct Frame {
    values: BTreeMap<ValueId, Value>,
    locals: BTreeMap<IRLocalId, Value>,
}

impl Frame {
    fn new() -> Self {
        Self {
            values: BTreeMap::new(),
            locals: BTreeMap::new(),
        }
    }
}

/// Lookup seam used by the per-instruction walker. Both
/// [`IRProgram`] and [`IRScript`] implement this so the same body
/// driver runs over either IR shape; only the underlying maps
/// differ. Function-call resolution + enum-decl lookup share the
/// same trait so each `EnumConstruct` arm has a registry-equivalent
/// handle for materializing the variant's `name` and (for struct
/// payloads) per-field names.
trait CallResolver {
    fn resolve(&self, mangled: &str) -> Option<&IRFunction>;
    fn enum_decl(&self, mangled: &str) -> Option<&IREnumDecl>;
    fn constant_value(&self, mangled: &str) -> Option<&IRConstantValue>;
}

impl CallResolver for IRProgram {
    fn resolve(&self, mangled: &str) -> Option<&IRFunction> {
        self.function(mangled)
    }

    fn enum_decl(&self, mangled: &str) -> Option<&IREnumDecl> {
        IRProgram::enum_decl(self, mangled)
    }

    fn constant_value(&self, mangled: &str) -> Option<&IRConstantValue> {
        IRProgram::constant_value(self, mangled)
    }
}

impl CallResolver for IRScript {
    fn resolve(&self, mangled: &str) -> Option<&IRFunction> {
        self.function(mangled)
    }

    fn enum_decl(&self, mangled: &str) -> Option<&IREnumDecl> {
        IRScript::enum_decl(self, mangled)
    }

    fn constant_value(&self, mangled: &str) -> Option<&IRConstantValue> {
        IRScript::constant_value(self, mangled)
    }
}

/// Run `function` in a fresh frame with `args` positionally bound to
/// its param `ValueId`s. Param promotion (entry-block `LocalDecl` +
/// `LocalWrite`) means the body reads from the slot, not the raw
/// param id; seeding `frame.values` keeps the promotion's
/// `LocalWrite { value: param.id }` resolvable. `@intrinsic`-tagged
/// functions route to [`crate::intrinsics`].
fn execute_function<R: CallResolver>(
    function: &IRFunction,
    args: Vec<Value>,
    resolver: &R,
) -> Result<Value, RuntimeError> {
    debug_assert_eq!(
        function.params.len(),
        args.len(),
        "arity mismatch calling `{}`: {} params vs {} args (typecheck invariant)",
        function.symbol,
        function.params.len(),
        args.len(),
    );
    match &function.kind {
        FunctionKind::Intrinsic => {
            return intrinsics::dispatch(function.symbol.mangled(), &args);
        }
        FunctionKind::Extern(_) => {
            return Err(RuntimeError::ExternNotSupported {
                symbol: function.symbol.mangled().to_string(),
            });
        }
        FunctionKind::Regular => {}
    }
    let mut frame = Frame::new();
    for (param, value) in function.params.iter().zip(args.into_iter()) {
        frame.values.insert(param.id, value);
    }

    execute_blocks(&function.blocks, &mut frame, resolver)
}

/// Drive a function body starting at `blocks[0]` until a `Return`
/// exits. The frame is shared across every block; unknown branch
/// targets panic per the seal contract.
fn execute_blocks<R: CallResolver>(
    blocks: &[IRBasicBlock],
    frame: &mut Frame,
    resolver: &R,
) -> Result<Value, RuntimeError> {
    let mut current = blocks
        .first()
        .expect("sealed function has at least one basic block")
        .id;
    loop {
        let block = find_block(blocks, current);
        for instruction in &block.instructions {
            execute_instruction(instruction, frame, resolver)?;
        }
        match &block.terminator {
            IRTerminator::Branch(target) => current = *target,
            IRTerminator::CondBranch {
                cond,
                then_block,
                else_block,
            } => {
                let cond_value = lookup(&frame.values, *cond)?;
                let Value::Bool(b) = cond_value else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!("cond_branch expects a Bool condition; got {cond_value}",),
                    });
                };
                current = if b { *then_block } else { *else_block };
            }
            IRTerminator::Return { value: None } => return Ok(Value::Unit),
            IRTerminator::Return { value: Some(id) } => return lookup(&frame.values, *id),
        }
    }
}

fn find_block(blocks: &[IRBasicBlock], id: IRBlockId) -> &IRBasicBlock {
    blocks
        .iter()
        .find(|b| b.id == id)
        .unwrap_or_else(|| panic!("interpreter: block `{id}` missing — seal invariant violation"))
}

fn execute_instruction<R: CallResolver>(
    instruction: &IRInstruction,
    frame: &mut Frame,
    resolver: &R,
) -> Result<(), RuntimeError> {
    match instruction {
        IRInstruction::BinaryOp { dest, lhs, op, rhs } => {
            let lhs_value = lookup(&frame.values, *lhs)?;
            let rhs_value = lookup(&frame.values, *rhs)?;
            let result = apply_binary_op(*op, lhs_value, rhs_value)?;
            frame.values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::Call { dest, callee, args } => {
            let mut arg_values = Vec::with_capacity(args.len());
            for arg in args {
                arg_values.push(lookup(&frame.values, *arg)?);
            }
            let callee_fn = resolver.resolve(callee.mangled()).unwrap_or_else(|| {
                panic!(
                    "interpreter: callee `{callee}` missing from IR — \
                     seal invariant violation",
                )
            });
            let result = execute_function(callee_fn, arg_values, resolver)?;
            frame.values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::Const { dest, value } => {
            frame.values.insert(*dest, materialize_const(value));
            Ok(())
        }
        IRInstruction::LoadConst {
            dest,
            const_id,
            ty: _,
        } => {
            let pooled = resolver.constant_value(const_id.mangled()).unwrap_or_else(|| {
                panic!(
                    "interpreter: LoadConst `{}` missing from pooled constants — seal invariant violation",
                    const_id.mangled(),
                )
            });
            let value = materialize_pooled_constant(pooled, resolver)?;
            frame.values.insert(*dest, value);
            Ok(())
        }
        IRInstruction::EnumConstruct {
            dest,
            payload,
            tag,
            ty,
        } => {
            let value = materialize_enum(ty, *tag, payload, frame, resolver)?;
            frame.values.insert(*dest, value);
            Ok(())
        }
        IRInstruction::FieldGet {
            base,
            dest,
            field_index,
            field_type: _,
            struct_symbol: _,
        } => {
            let base_value = lookup(&frame.values, *base)?;
            let Value::Struct { fields, .. } = base_value else {
                return Err(RuntimeError::TypeMismatch {
                    detail: format!("field_get expects a Struct receiver; got {base_value}",),
                });
            };
            let field = fields
                .into_iter()
                .nth(*field_index as usize)
                .unwrap_or_else(|| {
                    panic!(
                        "interpreter: FieldGet index {field_index} out of range — \
                         seal invariant violation",
                    )
                });
            frame.values.insert(*dest, field);
            Ok(())
        }
        // Slot identity comes from `LocalWrite`; `LocalDecl` is a
        // no-op for the interpreter (the LLVM backend uses it to
        // emit an entry-block alloca).
        IRInstruction::LocalDecl { .. } => Ok(()),
        IRInstruction::LocalRead { dest, local, .. } => {
            let value = frame.locals.get(local).cloned().unwrap_or_else(|| {
                panic!(
                    "interpreter: `LocalRead` of `{local}` before its `LocalWrite` — \
                     seal invariant violation",
                )
            });
            frame.values.insert(*dest, value);
            Ok(())
        }
        IRInstruction::LocalWrite { local, value } => {
            let resolved = lookup(&frame.values, *value)?;
            frame.locals.insert(*local, resolved);
            Ok(())
        }
        IRInstruction::StructInit { dest, fields, ty } => {
            let mut materialized = Vec::with_capacity(fields.len());
            for field in fields {
                materialized.push(lookup(&frame.values, field.value)?);
            }
            frame.values.insert(
                *dest,
                Value::Struct {
                    symbol: ty.clone(),
                    fields: materialized,
                },
            );
            Ok(())
        }
        IRInstruction::UnaryOp { dest, op, operand } => {
            let operand_value = lookup(&frame.values, *operand)?;
            let result = apply_unary_op(*op, operand_value)?;
            frame.values.insert(*dest, result);
            Ok(())
        }
    }
}

fn lookup(values: &BTreeMap<ValueId, Value>, id: ValueId) -> Result<Value, RuntimeError> {
    values
        .get(&id)
        .cloned()
        .ok_or(RuntimeError::ValueUndefined { id })
}

fn materialize_pooled_constant<R: CallResolver>(
    cv: &IRConstantValue,
    resolver: &R,
) -> Result<Value, RuntimeError> {
    match cv {
        IRConstantValue::Primitive(inner) => Ok(materialize_const(inner)),
        IRConstantValue::EnumVariant { tag, ty } => {
            let decl = resolver.enum_decl(ty.mangled()).unwrap_or_else(|| {
                panic!(
                    "interpreter: pooled enum `{}` missing from IR — seal invariant violation",
                    ty.mangled(),
                )
            });
            let variant = decl.variants.get(usize::from(tag.0)).unwrap_or_else(|| {
                panic!(
                    "interpreter: pooled EnumVariant `{}` references tag {:?} past {} variants — \
                         seal invariant violation",
                    ty.mangled(),
                    tag,
                    decl.variants.len(),
                )
            });
            Ok(Value::Enum {
                name: variant.name.clone(),
                payload: EnumPayload::Unit,
                symbol: ty.clone(),
                tag: *tag,
            })
        }
        IRConstantValue::Struct { fields, ty } => {
            let mut materialized = Vec::with_capacity(fields.len());
            for f in fields {
                materialized.push(materialize_pooled_constant(f, resolver)?);
            }
            Ok(Value::Struct {
                symbol: ty.clone(),
                fields: materialized,
            })
        }
    }
}

/// Materialize a [`Value::Enum`] from an `EnumConstruct` payload init.
/// Looks up the enum decl through the resolver, fetches the variant
/// at `tag.0` (constant-time index — seal asserts the tag is in
/// range and matches the payload shape), and zips the init values
/// with the variant's declared shape into an [`EnumPayload`].
///
/// Per-shape:
/// - Unit → `EnumPayload::Unit`.
/// - Tuple → materialize each `ValueId` against `frame.values`.
/// - Struct → zip the (canonicalized, declaration-order) inits with
///   the variant's declared `IRStructField`s so each materialized
///   value pairs with its declared field name in the resulting
///   `Vec<(String, Value)>`.
fn materialize_enum<R: CallResolver>(
    symbol: &IRSymbol,
    tag: IRVariantTag,
    payload: &EnumPayloadInit,
    frame: &Frame,
    resolver: &R,
) -> Result<Value, RuntimeError> {
    let decl = resolver.enum_decl(symbol.mangled()).unwrap_or_else(|| {
        panic!(
            "interpreter: enum `{symbol}` missing from IR — \
             seal invariant violation",
        )
    });
    let variant = decl.variants.get(usize::from(tag.0)).unwrap_or_else(|| {
        panic!(
            "interpreter: EnumConstruct on `{symbol}` references tag {tag} but the decl only \
             declares {} variant(s) — seal invariant violation",
            decl.variants.len(),
        )
    });
    let materialized = match (payload, &variant.payload) {
        (EnumPayloadInit::Unit, IRVariantPayload::Unit) => EnumPayload::Unit,
        (EnumPayloadInit::Tuple(ids), IRVariantPayload::Tuple(_)) => {
            let mut values = Vec::with_capacity(ids.len());
            for id in ids {
                values.push(lookup(&frame.values, *id)?);
            }
            EnumPayload::Tuple(values)
        }
        (EnumPayloadInit::Struct(inits), IRVariantPayload::Struct(declared)) => {
            let mut fields = Vec::with_capacity(inits.len());
            for (init, decl_field) in inits.iter().zip(declared.iter()) {
                let value = lookup(&frame.values, init.value)?;
                fields.push((decl_field.name.clone(), value));
            }
            EnumPayload::Struct(fields)
        }
        (init, declared) => panic!(
            "interpreter: EnumConstruct payload shape mismatch on `{symbol}.{}` \
             (declared {declared:?}, supplied {init:?}) — seal invariant violation",
            variant.name,
        ),
    };
    Ok(Value::Enum {
        name: variant.name.clone(),
        payload: materialized,
        symbol: symbol.clone(),
        tag,
    })
}

/// Materialize a `ConstValue` as a runtime [`Value`]. Every int
/// width collapses to `Value::Int(i64)` (the seal pass keeps
/// width-mismatched flows out, but the arms stay exhaustive).
fn materialize_const(value: &ConstValue) -> Value {
    match value {
        ConstValue::Bool(b) => Value::Bool(*b),
        ConstValue::Float32(v) => Value::Float32(*v),
        ConstValue::Float64(v) => Value::Float64(*v),
        ConstValue::Int8(v) => Value::Int(*v as i64),
        ConstValue::Int16(v) => Value::Int(*v as i64),
        ConstValue::Int32(v) => Value::Int(*v as i64),
        ConstValue::Int64(v) => Value::Int(*v),
        ConstValue::String(s) => Value::String(s.clone()),
        ConstValue::UInt8(v) => Value::Int(*v as i64),
        ConstValue::UInt16(v) => Value::Int(*v as i64),
        ConstValue::UInt32(v) => Value::Int(*v as i64),
        ConstValue::UInt64(v) => Value::Int(*v as i64),
        ConstValue::Unit => Value::Unit,
    }
}
