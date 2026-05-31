//! Return-mode catalog for `@intrinsic` functions. Intrinsic bodies
//! are empty, so the mode can't be inferred — it's hand-authored
//! here, keyed by the `[receiver, method]` identifier path.
//!
//! Covers the heap-data intrinsics, where ownership is observable:
//! genuine aliases (zero-cost reinterprets, element views) are
//! `Borrowed`; fresh allocations, clones, slices, and move-through
//! conversions are `Owned`. Anything not listed — including the
//! scalar / unit-returning intrinsics where ownership is moot —
//! falls through to the leak-safe `Borrowed` default.

use koja_ast::ast::ReturnMode;
use koja_ast::identifier::Identifier;

/// Mode for an `@intrinsic` named by `identifier`. Non-`[receiver,
/// method]` shapes (and any unlisted method) resolve to `Borrowed`.
pub(super) fn intrinsic_return_mode(identifier: &Identifier) -> ReturnMode {
    match identifier.path() {
        [receiver, method] => mode_for(receiver.as_str(), method.as_str()),
        _ => ReturnMode::Borrowed,
    }
}

fn mode_for(receiver: &str, method: &str) -> ReturnMode {
    use ReturnMode::{Borrowed, Owned};
    match (receiver, method) {
        ("Binary", "ptr" | "to_bits") => Borrowed,
        ("Binary", "byte_size" | "clone" | "to_string") => Owned,
        ("Bits", "to_binary") => Borrowed,
        ("Bits", "clone") => Owned,
        ("CPtr", "offset") => Borrowed,
        (
            "CPtr",
            "alloc" | "free" | "null" | "null?" | "read" | "to_binary" | "to_string" | "write",
        ) => Owned,
        ("List", "get" | "pop") => Borrowed,
        (
            "List",
            "append" | "concat" | "empty?" | "from_list" | "length" | "new" | "replace_at"
            | "slice",
        ) => Owned,
        ("Map", "get") => Borrowed,
        ("Map", "clone" | "empty?" | "from_map" | "has?" | "length" | "new" | "put" | "remove") => {
            Owned
        }
        ("String", "to_binary") => Borrowed,
        ("String", "byte_length" | "clone" | "get" | "length" | "slice" | "to_cstring") => Owned,
        _ => Borrowed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intrinsic(receiver: &str, method: &str) -> Identifier {
        Identifier::new("Koja", vec![receiver.to_string(), method.to_string()])
    }

    #[test]
    fn aliases_are_borrowed() {
        for (receiver, method) in [
            ("String", "to_binary"),
            ("Binary", "ptr"),
            ("Binary", "to_bits"),
            ("Bits", "to_binary"),
            ("CPtr", "offset"),
            ("List", "get"),
            ("List", "pop"),
            ("Map", "get"),
        ] {
            assert_eq!(
                intrinsic_return_mode(&intrinsic(receiver, method)),
                ReturnMode::Borrowed,
                "{receiver}.{method} aliases its input",
            );
        }
    }

    #[test]
    fn fresh_heap_is_owned() {
        for (receiver, method) in [
            ("String", "slice"),
            ("String", "clone"),
            ("List", "slice"),
            ("List", "from_list"),
            ("Map", "from_map"),
            ("CPtr", "to_string"),
            ("Binary", "to_string"),
        ] {
            assert_eq!(
                intrinsic_return_mode(&intrinsic(receiver, method)),
                ReturnMode::Owned,
                "{receiver}.{method} hands back fresh heap",
            );
        }
    }

    #[test]
    fn unknown_intrinsic_defaults_borrowed() {
        assert_eq!(
            intrinsic_return_mode(&intrinsic("Socket", "recv_from")),
            ReturnMode::Borrowed,
        );
        assert_eq!(
            intrinsic_return_mode(&Identifier::new("Koja", vec!["print".to_string()])),
            ReturnMode::Borrowed,
        );
    }
}
