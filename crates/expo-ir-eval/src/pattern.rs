//! Runtime helpers for `match` pattern instructions. The interpreter
//! has no pointers, so `subject_ptr` / `source_ptr` operands resolve
//! straight to runtime [`Value`]s via `Frame::materialize`.

use expo_ir::IROperand;
use expo_ir::resolved::patterns::ResolvedLiteral;

use crate::error::RuntimeError;
use crate::frame::Frame;
use crate::value::Value;

/// Compare the value referenced by `subject_ptr` against
/// [`ResolvedLiteral`] and produce `Value::Bool(equal)`. Type-incompatible
/// subjects (e.g. a string subject against an int literal) compare as
/// `false` rather than erroring; that matches codegen's
/// `emit_pattern_literal_eq` semantics where a structural mismatch
/// surfaces as a failed pattern-test, not an emission error.
pub(crate) fn pattern_literal_eq(
    frame: &Frame,
    subject_ptr: &IROperand,
    lit: &ResolvedLiteral,
) -> Result<Value, RuntimeError> {
    let subject = frame.materialize(subject_ptr)?;
    let equal = match lit {
        ResolvedLiteral::Bool(l) => subject.as_bool() == Some(*l),
        ResolvedLiteral::Float(l) => subject.as_float() == Some(*l),
        ResolvedLiteral::Int(l) => subject.as_int() == Some(*l),
        ResolvedLiteral::String(l) => subject.as_string() == Some(l.as_str()),
    };
    Ok(Value::Bool(equal))
}

/// Project field `field_index` out of the [`Value::Struct`] at
/// `subject_ptr`. The returned value lands at the IR-level `dest`,
/// where downstream `Pattern*` instructions read it as if it were a
/// freshly-projected pointer.
pub(crate) fn pattern_project_struct_field(
    frame: &Frame,
    subject_ptr: &IROperand,
    field_index: u32,
) -> Result<Value, RuntimeError> {
    let subject = frame.materialize(subject_ptr)?;
    let Value::Struct(struct_value) = subject else {
        return Err(RuntimeError::TypeMismatch(format!(
            "PatternProjectStructField: expected struct subject, got {subject:?}"
        )));
    };
    let index = field_index as usize;
    let (_, field_value) = struct_value.fields.get(index).ok_or_else(|| {
        RuntimeError::Unsupported(format!(
            "PatternProjectStructField: field index {index} out of bounds for {} (has {} fields)",
            struct_value.mangled,
            struct_value.fields.len()
        ))
    })?;
    Ok(field_value.clone())
}

/// Bind `locals[name]` to the value at `source_ptr`. Side-effecting;
/// no SSA `dest` -- subsequent `LoadLocal` reads address the binding
/// by name.
pub(crate) fn pattern_bind_from_ptr(
    frame: &mut Frame,
    name: &str,
    source_ptr: &IROperand,
) -> Result<(), RuntimeError> {
    let value = frame.materialize(source_ptr)?;
    frame.locals.insert(name.to_string(), value);
    Ok(())
}
