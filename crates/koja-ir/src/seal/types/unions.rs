//! Operand types for the union family: `UnionPayloadGet`,
//! `UnionTagGet`, and `UnionWrap`.

use crate::seal::seal_panic;
use crate::types::{IRType, ValueId};

use super::{TypeScope, registered, require_same_type};

/// The source must be a registered union with a member at
/// `member_index` whose type is the cached `member_type`.
pub(super) fn seal_union_payload_get(
    member_index: u8,
    member_type: &IRType,
    ty: &IRType,
    value: ValueId,
    scope: &TypeScope<'_>,
) {
    let owner = scope.owner;
    scope.require_value_type(value, ty, "UnionPayloadGet value");
    let IRType::Union { mangled, members } = ty else {
        seal_panic(&format!("{owner} UnionPayloadGet source is not a union"));
    };
    registered(scope.declarations.union_decl(mangled), "union", mangled);
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

pub(super) fn seal_union_tag_get(ty: &IRType, value: ValueId, scope: &TypeScope<'_>) {
    scope.require_value_type(value, ty, "UnionTagGet value");
}

/// The target must spell out the registered declaration, and the
/// wrapped value must carry the member type at `member_index`.
pub(super) fn seal_union_wrap(
    member_index: u8,
    member_type: &IRType,
    ty: &IRType,
    value: ValueId,
    scope: &TypeScope<'_>,
) {
    let owner = scope.owner;
    scope.require_value_type(value, member_type, "UnionWrap value");
    let IRType::Union { mangled, members } = ty else {
        seal_panic(&format!("{owner} UnionWrap target is not a union"));
    };
    let declaration = registered(scope.declarations.union_decl(mangled), "union", mangled);
    require_same_type(
        &IRType::Union {
            mangled: declaration.symbol.clone(),
            members: declaration.members.clone(),
        },
        ty,
        &format!("{owner} UnionWrap declaration"),
    );
    let Some(expected) = members.get(member_index as usize) else {
        seal_panic(&format!(
            "{owner} UnionWrap member index {member_index} is out of range"
        ));
    };
    require_same_type(member_type, expected, &format!("{owner} UnionWrap member"));
}
