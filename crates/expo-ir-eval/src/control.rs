//! Block-graph control flow for the [`crate::Interp`] backend.
//!
//! Houses the per-call control primitives -- terminator interpretation,
//! parameter binding, and block lookup -- plus the [`ControlFlow`]
//! signal that [`crate::Interp::execute_blocks`] consumes to decide
//! whether to follow an edge or unwind with a return value.

use expo_ir::{IRBasicBlock, IRBlockId, IRParam, IRTerminator};

use crate::error::RuntimeError;
use crate::frame::Frame;
use crate::value::Value;

/// Outcome of evaluating a basic block's terminator. Either jump to a
/// successor block or unwind from the current call with the returned
/// value.
pub(crate) enum ControlFlow {
    Goto(IRBlockId),
    Return(Value),
}

/// Bind incoming `args` to the parameter slot names declared by `params`.
/// `IRParam::Self_` always binds to the local name `self`; regular
/// parameters bind to their declared name.
pub(crate) fn bind_params(frame: &mut Frame, params: &[IRParam], args: Vec<Value>) {
    let mut iter = args.into_iter();
    for param in params {
        let Some(value) = iter.next() else {
            break;
        };
        match param {
            IRParam::Regular { name } => {
                frame.locals.insert(name.clone(), value);
            }
            IRParam::Self_ => {
                frame.locals.insert("self".to_string(), value);
            }
        }
    }
}

/// Look up a basic block by id within a function's block list.
pub(crate) fn block_by_id(
    blocks: &[IRBasicBlock],
    id: IRBlockId,
) -> Result<&IRBasicBlock, RuntimeError> {
    blocks
        .iter()
        .find(|block| block.id == id)
        .ok_or(RuntimeError::UnknownBlock(id))
}

/// Evaluate `terminator` against `frame` and return the next
/// [`ControlFlow`] step. `Branch` always jumps; `CondBranch`
/// materializes the predicate and selects a successor; `Return`
/// materializes the optional value (defaulting to `Unit`) and unwinds;
/// `Unreachable` raises [`RuntimeError::Unreachable`].
pub(crate) fn follow_terminator(
    frame: &Frame,
    terminator: &IRTerminator,
) -> Result<ControlFlow, RuntimeError> {
    match terminator {
        IRTerminator::Branch(target) => Ok(ControlFlow::Goto(*target)),
        IRTerminator::CondBranch {
            cond,
            then,
            otherwise,
        } => {
            let value = frame.materialize(cond)?;
            let taken = value
                .as_bool()
                .ok_or_else(|| RuntimeError::TypeMismatch("CondBranch cond not Bool".into()))?;
            Ok(ControlFlow::Goto(if taken { *then } else { *otherwise }))
        }
        IRTerminator::Return { value, .. } => {
            let returned = match value {
                Some(operand) => frame.materialize(operand)?,
                None => Value::Unit,
            };
            Ok(ControlFlow::Return(returned))
        }
        IRTerminator::Unreachable => Err(RuntimeError::Unreachable),
    }
}
