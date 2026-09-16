//! Hints for stdlib globals that were renamed. When a lookup misses
//! on one of these names, the diagnostic says where the name went so
//! a stale program points at its own fix.

/// Old `Global.*` names paired with the name that replaced them.
const RENAMED_GLOBALS: &[(&str, &str)] = &[("DateTime", "Timestamp")];

/// The hint for a missed lookup on `name`, when `name` is a renamed
/// stdlib global.
pub(crate) fn rename_hint(name: &str) -> Option<String> {
    RENAMED_GLOBALS
        .iter()
        .find(|(old, _)| *old == name)
        .map(|(old, new)| format!("`{old}` was renamed to `{new}`"))
}
