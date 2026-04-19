//! Resolved metadata for the process / spawn / receive lowering paths.
//!
//! Lowering decides envelope types, msg/reply pairs, and arm
//! classification for tagged receives; emission builds the LLVM
//! `expo_rt_*` calls and arm dispatch.

use expo_ast::ast::MatchArm;
use expo_ast::types::Type;

/// Result of [`crate::lower::processes::resolve_receive`]: the envelope
/// type the receive should bind, plus whether a timeout arm is present.
pub struct ResolvedReceive {
    pub envelope_type: Type,
    pub has_timeout: bool,
}

/// Computed `Ref<M, R>` metadata: mangled monomorphized name plus the
/// resulting Expo type.
pub struct ResolvedRefType {
    pub expo_type: Type,
    pub mangled_name: String,
    pub msg_type: Type,
    pub reply_type: Type,
}

/// Tagged receive arm classification: lowering partitions match arms into
/// IO-ready, lifecycle, and business buckets so emission can build the
/// envelope dispatch.
pub struct ResolvedTaggedReceive<'a> {
    pub business_arms: Vec<&'a MatchArm>,
    pub envelope_type: Type,
    pub io_ready_arms: Vec<&'a MatchArm>,
    pub lifecycle_arms: Vec<&'a MatchArm>,
    pub m_has_io_ready: bool,
}
