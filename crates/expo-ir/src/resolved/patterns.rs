//! Resolved match patterns: the decision a `match` arm makes about how to
//! test a subject and which bindings to introduce, with all package-aware
//! type-key resolution and variant-shape lookups already performed.
//!
//! Lowering (in `expo-codegen`) consumes the AST `Pattern` plus the subject's
//! `Type` and produces a `ResolvedPattern`. Emission then walks the resolved
//! tree calling only LLVM builder operations -- no registry lookups, no
//! string-key reverse engineering.
//!
//! ## Invariant: payload extraction is structurally constrained
//!
//! `EnumUnit` carries no payload information. Emission has no way to ask for
//! payload GEP / load on a unit variant -- the field doesn't exist. This is
//! the property that makes the previously-deferred `GEPIndex` panic at
//! `payload_ptr` GEP unreachable: a unit variant cannot be lowered into a
//! `ResolvedPattern` shape that asks for one.

use expo_ast::ast::BinarySegment;
use expo_ast::types::Type;

use crate::identity::MonomorphizedTypeIdentifier;

/// A literal value that can appear inside a pattern, with its raw source
/// already parsed into the runtime form needed for comparison.
#[derive(Clone, Debug)]
pub enum ResolvedLiteral {
    Bool(bool),
    Float(f64),
    Int(i64),
    /// String literal -- emitted as a global pointer and compared with `strcmp`.
    String(String),
}

/// A resolved field within an `EnumStruct` pattern. The field index has been
/// looked up against the variant's declared shape so emission can GEP without
/// re-querying the type registry.
pub struct ResolvedFieldPattern {
    /// The source-level field name (also the binding name when `sub` is `None`).
    pub name: String,
    /// The zero-based field index within the variant payload struct.
    pub field_index: u32,
    /// The Expo type of this field (as declared on the variant).
    pub field_type: Type,
    /// Optional nested pattern. `None` means "bind only" -- no further test.
    pub sub: Option<ResolvedPattern>,
}

/// A pattern after resolution: package-qualified enum keys, looked-up tags,
/// and known variant shapes. No string-key lookups required during emission.
pub enum ResolvedPattern {
    /// Wildcard `_` -- always matches, introduces no bindings.
    AlwaysMatch,
    /// Variable binding `x` (or typed binding where the resolved type matches
    /// the subject) -- always matches and binds the subject value.
    ///
    /// `strict_llvm` controls how a missing LLVM type translation is reported:
    /// plain `Pattern::Binding` falls back to `i8` (the subject may be a
    /// `Type::Unknown`), whereas a typed binding (`p: Post`) errors so the
    /// user gets a clear "unsupported type" diagnostic at compile time.
    Bind {
        name: String,
        ty: Type,
        strict_llvm: bool,
    },
    /// Literal comparison `42`, `"hello"`, etc.
    LiteralEq {
        lit: ResolvedLiteral,
        /// The Expo type of the subject the literal is compared against.
        /// Needed to compute the correct LLVM load type.
        subject_ty: Type,
    },
    /// A unit enum variant `Color.Red`. No payload exists -- emission only
    /// performs a tag check.
    ///
    /// Carries no payload fields, intentionally. Emission cannot ask for a
    /// payload GEP for a unit variant because there's nothing to GEP into;
    /// the previously-deferred `GEPIndex` panic at the payload pointer is
    /// unreachable from this arm by construction.
    EnumUnit {
        /// The LLVMTypeCache key (package-qualified or mangled) for the enum.
        enum_key: String,
        variant: String,
        tag: u8,
    },
    /// A tuple-payload enum variant `Option.Some(x)` or shorthand `Some(x)`.
    EnumTuple {
        enum_key: String,
        variant: String,
        tag: u8,
        /// Each element's declared type plus its sub-pattern, in source order.
        elements: Vec<(Type, ResolvedPattern)>,
    },
    /// A struct-payload enum variant `Shape.Rect { width, height }`.
    EnumStruct {
        enum_key: String,
        variant: String,
        tag: u8,
        fields: Vec<ResolvedFieldPattern>,
    },
    /// A typed binding into a union member (`p: Post`). Performs a tag check
    /// against the union's discriminant for the member type, then binds the
    /// unwrapped value.
    UnionMember {
        /// Mangled key for the union type (the subject).
        union_mangled: MonomorphizedTypeIdentifier,
        /// Mangled key for the member type being matched.
        member_mangled: MonomorphizedTypeIdentifier,
        /// Discriminant tag for the member within the union, looked up at
        /// lowering time so emission performs no name resolution.
        tag: u8,
        /// Resolved Expo type of the member (used for the bind LLVM type).
        member_ty: Type,
        /// The binding name introduced when the test succeeds.
        bind_name: String,
    },
    /// Disjunction `p1 | p2 | p3` -- matches if any sub-pattern matches.
    /// Bindings inside `Or` are not safe to use in arm bodies (this matches
    /// the legacy behavior); the resolved tree just records the structure.
    Or(Vec<ResolvedPattern>),
    /// Binary/bitstring pattern. Lowering for binary segments stays in the
    /// codegen `binary` module; this variant is a passthrough handle so the
    /// rest of the pattern tree can compose with it uniformly.
    Binary { segments: Vec<BinarySegment> },
}
