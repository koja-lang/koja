//! Stub-retirement diagnostic helpers.
//!
//! The lowering layer publishes two inventories that drive the
//! Stub-retirement workflow (see `expo/stub/stub-categorization.md`):
//!
//! - **`[STUB-FALLTHROUGH]`** -- one entry per
//!   `(ExprKind, function, span)` triple that fell through to
//!   [`crate::values::IRInstruction::Stub`]. Emitted by
//!   [`log_stub_fallthrough`] from the universal fall-through arm of
//!   [`crate::Lowerer::lower_expr_to_operand`].
//! - **`[HELPER-BAIL]`** -- one entry per `(helper, reason, function,
//!   span)` quadruple that explains *why* a Category-A helper
//!   (`lower_method_call_or_stub`, `lower_ident_or_stub`,
//!   `lower_call_or_stub`, ...) returned `None` and routed its caller
//!   to the Stub fall-through. Emitted by [`log_helper_bail`].
//!
//! Both inventories share a single process-wide [`StubFallthroughSink`]
//! and dedupe set, gated by the `EXPO_STUB_INVENTORY_FILE` env var. The
//! file backing avoids the pipe-buffer deadlock that subprocess-based
//! test harnesses (e.g. `expo-driver`'s `lang_suite`) would hit if we
//! routed thousands of inventory lines to stderr -- their parent
//! process only drains stderr after the child exits, so a high-volume
//! `eprintln!` would block the child indefinitely. Routing to a file
//! keeps the child unblocked; harnesses opt in by setting the env var
//! before running tests (see `expo/stub/regenerate.sh`).
//!
//! Once every `ExprKind` has a typed lowering and every Category-A
//! helper either drains its bail set or migrates to a typed
//! instruction, this whole module retires alongside
//! [`crate::values::IRInstruction::Stub`] itself.

use std::io::Write;
use std::sync::OnceLock;

use expo_ast::span::Span;

const FALLBACK_FUNCTION: &str = "<unknown>";

/// Helper tag constants for [`log_helper_bail`]. Alpha-sorted per
/// build.mdc "alpha-sort enum variants and match arms where the
/// ordering is arbitrary".
pub const HELPER_CALL: &str = "lower_call_or_stub";
pub const HELPER_IDENT: &str = "lower_ident_or_stub";
pub const HELPER_METHOD_CALL: &str = "lower_method_call_or_stub";
pub const HELPER_STATIC_CALL: &str = "lower_static_call_or_stub";

/// Bail reasons for [`HELPER_CALL`]. Alpha-sorted.
///
/// Slice 2 retired `args-need-coercion` by lifting it into
/// [`crate::values::IRInstruction::UnionWrap`] via
/// [`crate::Lowerer::stage_arg_coercions`], same shape as the Slice 1
/// `MethodCall` lift; future `Coercion` variants add their own reason
/// constants only if they re-introduce a bail.
pub const REASON_CALL_NON_DIRECT_ROUTE: &str = "non-direct-route";
pub const REASON_CALL_NON_IDENT_CALLEE: &str = "non-ident-callee";
pub const REASON_CALL_NO_RESOLVED_FUNCTION: &str = "no-resolved-function";
pub const REASON_CALL_PENDING_MONO: &str = "pending-mono";
pub const REASON_CALL_STRUCT_OR_ENUM_CTOR: &str = "struct-or-enum-ctor";

/// Bail reasons for [`HELPER_IDENT`]. Alpha-sorted.
pub const REASON_IDENT_NO_BINDING: &str = "no-binding";

/// Bail reasons for [`HELPER_METHOD_CALL`]. Alpha-sorted.
///
/// Slice 1 (Wave 32) retired `args-need-coercion` and
/// `receiver-coercion` by lifting both into
/// [`crate::values::IRInstruction::UnionWrap`] via
/// [`crate::Lowerer::stage_union_widen`]; future `Coercion` variants
/// add their own reason constants only if they re-introduce a bail.
pub const REASON_METHOD_CALL_CLONE_SHORTCUT: &str = "clone-shortcut";
pub const REASON_METHOD_CALL_NO_IMPL_METHOD: &str = "no-impl-method";
pub const REASON_METHOD_CALL_NO_RESOLVED_RECEIVER_TYPE: &str = "no-resolved-receiver-type";
pub const REASON_METHOD_CALL_PENDING_MONO: &str = "pending-mono";
pub const REASON_METHOD_CALL_RESOLVE_METHOD_CALL_FAILED: &str = "resolve-method-call-failed";
pub const REASON_METHOD_CALL_RESOLVE_STRUCT_NAME_FAILED: &str = "resolve-struct-name-failed";
pub const REASON_METHOD_CALL_STATIC_CALL_ROUTE: &str = "static-call-route";
pub const REASON_METHOD_CALL_UNREGISTERED_MANGLED_NAME: &str = "unregistered-mangled-name";

/// Bail reasons for [`HELPER_STATIC_CALL`]. Alpha-sorted.
pub const REASON_STATIC_CALL_PENDING_METHOD_MONO: &str = "pending-method-mono";
pub const REASON_STATIC_CALL_PENDING_TYPE_MONO: &str = "pending-type-mono";
pub const REASON_STATIC_CALL_RESOLVE_FAILED: &str = "resolve-static-call-failed";
pub const REASON_STATIC_CALL_UNREGISTERED_MANGLED_NAME: &str = "unregistered-mangled-name";

/// Dedupe key for the [`STUB-FALLTHROUGH`] inventory:
/// `(expr_kind_name, function-being-lowered, span debug repr)`.
type StubFallthroughKey = (&'static str, String, String);

/// Dedupe key for the [`HELPER-BAIL`] inventory:
/// `(helper, reason, function, span debug repr)`.
type HelperBailKey = (&'static str, &'static str, String, String);

/// State for the inventory dedupe sets.
///
/// Keyed-by-tag separation means a span that fell through to Stub via
/// multiple distinct helpers shows up once per `(helper, reason)` in
/// `[HELPER-BAIL]` and once in `[STUB-FALLTHROUGH]` -- the inventory
/// stays scannable instead of degenerating to one line per call site.
struct SinkState {
    file: std::fs::File,
    stub_seen: std::collections::HashSet<StubFallthroughKey>,
    helper_seen: std::collections::HashSet<HelperBailKey>,
}

/// Process-wide sink for inventory lines.
///
/// `Disabled` is the default (and the value used when
/// `EXPO_STUB_INVENTORY_FILE` is unset, or when the named file can't
/// be opened): logging is a no-op and the helpers impose no overhead
/// beyond a [`OnceLock`] read. `File` carries an opened append-handle
/// plus the dedupe sets so a single compiler process only writes each
/// unique key once.
enum StubFallthroughSink {
    Disabled,
    File(std::sync::Mutex<SinkState>),
}

/// One-shot accessor for the process-wide sink. Both
/// [`log_stub_fallthrough`] and [`log_helper_bail`] route through here
/// so they share initialization, the env-var gate, and the dedupe
/// state.
fn sink() -> &'static StubFallthroughSink {
    static SINK: OnceLock<StubFallthroughSink> = OnceLock::new();
    SINK.get_or_init(|| match std::env::var_os("EXPO_STUB_INVENTORY_FILE") {
        Some(path) => match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(file) => StubFallthroughSink::File(std::sync::Mutex::new(SinkState {
                file,
                stub_seen: Default::default(),
                helper_seen: Default::default(),
            })),
            Err(_) => StubFallthroughSink::Disabled,
        },
        None => StubFallthroughSink::Disabled,
    })
}

/// Log the first sighting of an [`ExprKind`] shape that fell through
/// to [`crate::values::IRInstruction::Stub`], deduplicating across the
/// process by `(expr_kind_name, function, span)`. Subsequent sightings
/// of the same triple stay silent so a `cargo test` run produces a
/// scannable inventory instead of a deluge.
///
/// Used by `Lowerer::lower_expr_to_operand_with_tail`'s fall-through
/// arm. See module docs for the broader workflow.
pub fn log_stub_fallthrough(kind: &'static str, function: Option<&str>, span: &Span) {
    let StubFallthroughSink::File(state) = sink() else {
        return;
    };
    let function = function.unwrap_or(FALLBACK_FUNCTION);
    let key = (kind, function.to_string(), format!("{span:?}"));
    let mut guard = state
        .lock()
        .expect("[STUB-FALLTHROUGH] sink mutex poisoned");
    if guard.stub_seen.insert(key) {
        let _ = writeln!(
            guard.file,
            "[STUB-FALLTHROUGH] ExprKind::{kind} in {function} at {span:?}"
        );
    }
}

/// Log the first sighting of a Category-A helper bail (`Ok(None)`
/// return) tagged with the reason it bailed, deduplicating by
/// `(helper, reason, function, span)`. Pairs with
/// [`log_stub_fallthrough`]: every helper bail tagged here is followed
/// by a Stub fall-through emitted from the universal arm in
/// [`crate::lower::values`], so the two inventories cross-reference.
pub fn log_helper_bail(
    helper: &'static str,
    reason: &'static str,
    function: Option<&str>,
    span: &Span,
) {
    let StubFallthroughSink::File(state) = sink() else {
        return;
    };
    let function = function.unwrap_or(FALLBACK_FUNCTION);
    let key = (helper, reason, function.to_string(), format!("{span:?}"));
    let mut guard = state.lock().expect("[HELPER-BAIL] sink mutex poisoned");
    if guard.helper_seen.insert(key) {
        let _ = writeln!(
            guard.file,
            "[HELPER-BAIL] {helper} reason={reason} in {function} at {span:?}"
        );
    }
}
