//! Runtime errors raised by the interpreter.

use expo_ir::{FunctionIdentifier, ProgramInvariantError};

#[derive(Debug, Clone)]
pub enum RuntimeError {
    /// The IR program failed [`expo_ir::IRProgram::validate`].
    InvalidProgram(ProgramInvariantError),
    /// `panic("...")` was called from Expo source.
    Panic(String),
    /// A `Call` / `MethodCall` referenced a callee not in `IRProgram.functions`.
    UnknownCallee(FunctionIdentifier),
    /// An `IRInstruction::LoadLocal` referenced a name not in the current frame.
    UndefinedLocal(String),
    /// An `IROperand::Local` referenced a value id with no entry in the current frame.
    UndefinedValue(expo_ir::IRValueId),
    /// An `IRInstruction::Stub` survived to interpretation. Lowering has a gap.
    StubReached(String),
    /// `IRFunctionKind::Extern` invoked. FFI is deferred.
    ExternNotSupported(FunctionIdentifier),
    /// An intrinsic was invoked that the interpreter doesn't yet implement.
    UnknownIntrinsic { base_type: String, method: String },
    /// A binary operation got operands of mismatched / unsupported types.
    TypeMismatch(String),
    /// A control-flow operation referenced a block id that wasn't in the function.
    UnknownBlock(expo_ir::IRBlockId),
    /// A terminator was reached that the interpreter rejects (`Unreachable`).
    Unreachable,
    /// Catch-all for IR shapes the interpreter doesn't yet handle.
    Unsupported(String),
}

impl From<ProgramInvariantError> for RuntimeError {
    fn from(error: ProgramInvariantError) -> Self {
        RuntimeError::InvalidProgram(error)
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::InvalidProgram(e) => write!(f, "invalid IR program: {e}"),
            RuntimeError::Panic(msg) => write!(f, "panic: {msg}"),
            RuntimeError::UnknownCallee(id) => write!(f, "unknown callee: {id}"),
            RuntimeError::UndefinedLocal(name) => write!(f, "undefined local: `{name}`"),
            RuntimeError::UndefinedValue(id) => write!(f, "undefined SSA value: {id:?}"),
            RuntimeError::StubReached(detail) => {
                write!(f, "interpreter encountered IRInstruction::Stub: {detail}")
            }
            RuntimeError::ExternNotSupported(id) => {
                write!(f, "extern function not supported by interpreter: {id}")
            }
            RuntimeError::UnknownIntrinsic { base_type, method } => {
                write!(f, "intrinsic not implemented: {base_type}.{method}")
            }
            RuntimeError::TypeMismatch(detail) => write!(f, "type mismatch: {detail}"),
            RuntimeError::UnknownBlock(id) => write!(f, "unknown block: {id:?}"),
            RuntimeError::Unreachable => write!(f, "reached IRTerminator::Unreachable"),
            RuntimeError::Unsupported(detail) => write!(f, "unsupported: {detail}"),
        }
    }
}

impl std::error::Error for RuntimeError {}
