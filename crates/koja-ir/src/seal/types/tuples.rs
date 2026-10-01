//! Operand types for the tuple family: `TupleGet` and `TupleInit`.

use crate::seal::seal_panic;
use crate::types::{IRType, ValueId};

use super::{TypeScope, require_same_type};

/// The base must be a tuple with an element at `index` whose type
/// is the cached `element_type`.
pub(super) fn seal_tuple_get(
    base: ValueId,
    element_type: &IRType,
    index: u32,
    scope: &TypeScope<'_>,
) {
    let owner = scope.owner;
    let IRType::Tuple(elements) = scope.value_type(base) else {
        seal_panic(&format!("{owner}: TupleGet base `{base}` is not a tuple"));
    };
    let Some(declared) = elements.get(index as usize) else {
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

/// The elements must match the tuple type in count and in type.
pub(super) fn seal_tuple_init(elements: &[ValueId], ty: &[IRType], scope: &TypeScope<'_>) {
    if elements.len() != ty.len() {
        seal_panic(&format!(
            "{}: TupleInit carries {got} element(s) but its type has {expected}",
            scope.owner,
            got = elements.len(),
            expected = ty.len(),
        ));
    }
    for (element, expected) in elements.iter().zip(ty) {
        scope.require_value_type(*element, expected, "TupleInit element");
    }
}
