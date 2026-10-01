//! Operand types for the process family: `Receive` and `Spawn`.

use crate::function::{ReceiveAfter, ReceiveArm};
use crate::types::{IRType, ValueId};

use super::TypeScope;

/// The timeout, when present, is an `Int64`, and each arm's payload
/// local is declared with the arm's payload type.
pub(super) fn seal_receive(
    after: Option<&ReceiveAfter>,
    arms: &[ReceiveArm],
    scope: &TypeScope<'_>,
) {
    if let Some(after) = after {
        scope.require_value_type(after.timeout, &IRType::Int64, "Receive timeout");
    }
    for arm in arms {
        scope.require_local_type(arm.payload_local, &arm.payload_type, "Receive payload");
    }
}

pub(super) fn seal_spawn(config: ValueId, config_type: &IRType, scope: &TypeScope<'_>) {
    scope.require_value_type(config, config_type, "Spawn config");
}
