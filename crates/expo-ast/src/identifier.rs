use std::fmt;

use crate::ast::PassMode;

/// Which package a type belongs to. Used by [`TypeIdentifier`] to distinguish
/// types with the same name from different packages.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Package {
    /// The built-in standard library (auto-imported).
    Global,
    /// A named package (e.g. `JSON`, `Net`, or the user's project name).
    Named(String),
    /// Package not yet determined. Present only during early pipeline stages;
    /// resolved to a concrete package before codegen.
    Unresolved,
}

impl Package {
    /// Builds a package-qualified key (e.g. `"Global.List"` or `"Alpha.Config"`)
    /// suitable for the `name_index` reverse lookup. Returns `None` for
    /// [`Package::Unresolved`] since unresolved packages have no scope to
    /// qualify against.
    pub fn qualify(&self, name: &str) -> Option<String> {
        match self {
            Package::Global => Some(format!("Global.{name}")),
            Package::Named(p) => Some(format!("{p}.{name}")),
            Package::Unresolved => None,
        }
    }
}

impl fmt::Display for Package {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Package::Global => write!(f, "Global"),
            Package::Named(name) => write!(f, "{name}"),
            Package::Unresolved => Ok(()),
        }
    }
}

/// AST-wide identifier for any globally-named entity (struct, enum, function,
/// method, variant, etc.). Carries the package name and the lexical
/// containment path within that package (e.g. `["User", "validate"]` for a
/// method on `User`).
///
/// Opaque by design: callers never reach inside, they ask via contract
/// methods (`is_in_package`, `is_in_global`, `qualified_name`, ...). Internal
/// representation can evolve without breaking consumers.
///
/// An `Identifier` is by construction a *resolved* global -- there is no
/// in-flight or sentinel state inside it. The "not yet resolved" case lives
/// at the AST node level via [`Resolution::Unresolved`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Identifier {
    package: String,
    path: Vec<String>,
}

impl Identifier {
    /// Canonical constructor. Panics on empty package or empty path -- both
    /// would represent malformed identifiers that callers should never
    /// produce.
    pub fn new(package: impl Into<String>, path: Vec<String>) -> Self {
        let package = package.into();
        assert!(!package.is_empty(), "Identifier package cannot be empty");
        assert!(!path.is_empty(), "Identifier path cannot be empty");
        Self { package, path }
    }

    pub fn package(&self) -> &str {
        &self.package
    }

    pub fn path(&self) -> &[String] {
        &self.path
    }

    /// The last segment of the path -- the "short name" of the identifier
    /// (e.g. `"validate"` for `User.validate`).
    pub fn last(&self) -> &str {
        self.path.last().expect("path is non-empty by construction")
    }

    /// `package.A.B.C` -- the canonical fully-qualified rendering, used as
    /// a stable string key (e.g. for mangling, debug output, diagnostics).
    pub fn qualified_name(&self) -> String {
        format!("{}.{}", self.package, self.path.join("."))
    }

    pub fn is_in_package(&self, pkg: &str) -> bool {
        self.package == pkg
    }

    pub fn is_in_global(&self) -> bool {
        self.package == "Global"
    }
}

impl fmt::Display for Identifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.qualified_name())
    }
}

/// Opaque handle into the alpha typecheck crate's `GlobalRegistry`.
///
/// Assigned by the registry at insertion time (sequential `u32`s in the
/// current implementation). Callers treat it as opaque: they never
/// synthesize one by hand and never reason about its numeric value.
/// The constructor [`GlobalRegistryId::new`] is public so the registry
/// crate can mint ids, but outside of the registry itself there should
/// be no reason to call it.
///
/// The id's derivation is an implementation detail; a future parallel
/// cache will swap sequential assignment for content-addressable
/// hashing without changing this type's surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GlobalRegistryId(u32);

impl GlobalRegistryId {
    /// Wraps a raw `u32`. Intended for registry internals only.
    pub fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the raw `u32` for serialization or debug rendering.
    pub fn as_u32(self) -> u32 {
        self.0
    }
}

impl fmt::Display for GlobalRegistryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Opaque per-function handle for a local binding (parameter or
/// `let`-introduced variable). Minted by alpha typecheck's `LocalScope`
/// when a fresh name enters scope; carried by [`Resolution::Local`] on
/// every reference site to that binding within the same function.
///
/// Mirrors [`GlobalRegistryId`]: a public [`Self::new`] ctor (so the
/// typecheck crate can mint ids), a public [`Self::as_u32`] accessor
/// (so the IR-side translator can derive its parallel handle), and
/// nothing else. Outside of those two seams the handle is opaque.
///
/// `LocalId` does **not** cross the IR boundary. The IR crate defines
/// a sibling `IRLocalId` and translates one-to-one at lower time, so
/// eval and codegen consume only IR types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalId(u32);

impl LocalId {
    /// Wraps a raw `u32`. Intended for typecheck `LocalScope` internals only.
    pub fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the raw `u32` for the IR-side translator and diagnostic rendering.
    pub fn as_u32(self) -> u32 {
        self.0
    }
}

impl fmt::Display for LocalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Opaque per-decl handle for a type parameter binding (the `T` in
/// `struct Pair<T, U>`). Mirrors [`LocalId`] / [`GlobalRegistryId`]:
/// the registry / decl owns the data, callers carry only the handle.
/// Index into the owning decl's `type_params` Vec.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeParamIndex(u32);

impl TypeParamIndex {
    pub fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub fn as_u32(self) -> u32 {
        self.0
    }
}

impl fmt::Display for TypeParamIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Resolution attached to an AST reference site by typecheck.
///
/// `Global` / `Local` carry registry / `LocalScope` handles.
/// `TypeParam` is the same idea anchored to a generic decl: `owner`
/// is the [`GlobalRegistryId`] of the enclosing struct/enum, `index`
/// picks one of its `type_params`. Seal asserts the variant only
/// appears inside generic-decl bodies. `Unresolved` is the in-flight
/// state before resolve runs.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Resolution {
    Global(GlobalRegistryId),
    Local(LocalId),
    TypeParam {
        owner: GlobalRegistryId,
        index: TypeParamIndex,
    },
    #[default]
    Unresolved,
}

impl Resolution {
    pub fn is_resolved(&self) -> bool {
        matches!(
            self,
            Resolution::Global(_) | Resolution::Local(_) | Resolution::TypeParam { .. }
        )
    }
}

/// Northstar-aligned type annotation attached to every `Expr` by alpha
/// typecheck.
///
/// Split along the named/anonymous axis:
///
/// - [`Self::Named`] — types with a source-given name. Identity is the
///   head [`Resolution`] (which registry entry this refers to);
///   `type_args` are the generic arguments at this use site
///   (themselves `ResolvedType`s, so generics nest).
/// - [`Self::Anonymous`] — structural types with no source name.
///   Identity is by shape. Today: function types only; future: records,
///   tuples.
/// - [`Self::Unresolved`] — in-flight placeholder before resolve runs.
///   `Default` returns this.
///
/// Shape examples:
/// - `Int` -> `Named { Global(int_id), [] }`
/// - `List<Int>` -> `Named { Global(list_id), [Named { Global(int_id), [] }] }`
/// - `fn (Int) -> Bool` -> `Anonymous(Function { [Int], Bool })`
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ResolvedType {
    /// Anonymous structural type. Identity by shape, never the target
    /// of a trait impl, no canonical owner.
    Anonymous(AnonymousKind),

    /// Named type with a head `resolution` and zero or more type
    /// arguments.
    Named {
        resolution: Resolution,
        type_args: Vec<ResolvedType>,
    },

    /// In-flight placeholder before resolve runs.
    #[default]
    Unresolved,
}

impl ResolvedType {
    /// Fully-unresolved placeholder. Equivalent to
    /// [`ResolvedType::default`]; exposed as a named constructor for
    /// intent at call sites.
    pub fn unresolved() -> Self {
        Self::Unresolved
    }

    /// Leaf [`Self::Named`] node: a head `resolution` and no type
    /// arguments. Convenience for primitives and other arity-0 types.
    pub fn leaf(resolution: Resolution) -> Self {
        Self::Named {
            resolution,
            type_args: Vec::new(),
        }
    }

    /// True iff every leaf is resolved. Seal uses this as its
    /// whole-tree invariant — a single [`Self::Unresolved`] hole or a
    /// [`Resolution::Unresolved`] head anywhere in the tree fails the
    /// check.
    pub fn is_resolved(&self) -> bool {
        match self {
            Self::Anonymous(AnonymousKind::Function { params, ret }) => {
                params.iter().all(|p| p.ty.is_resolved()) && ret.is_resolved()
            }
            Self::Named {
                resolution,
                type_args,
            } => resolution.is_resolved() && type_args.iter().all(Self::is_resolved),
            Self::Unresolved => false,
        }
    }
}

/// Kind tag for [`ResolvedType::Anonymous`]. Each variant is an
/// anonymous type family with its own structural-equivalence rule.
///
/// Today: only [`Self::Function`]. Future: `Record { fields }` and
/// `Tuple { elements }`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AnonymousKind {
    /// `fn (T, U) -> R` — structural function type with per-parameter
    /// pass mode. Equates positionally on params (mode + type) and
    /// covariantly on return.
    Function {
        params: Vec<FnParam>,
        ret: Box<ResolvedType>,
    },
}

/// A single parameter of an [`AnonymousKind::Function`]: a type plus
/// the surface pass mode (`move` / `Borrow` / `Copy`).
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FnParam {
    pub mode: PassMode,
    pub ty: ResolvedType,
}

/// A canonical, package-qualified identifier for a user-defined type.
/// Every struct, enum, and protocol carries one of these throughout the
/// compiler pipeline, ensuring types from different packages never collide.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeIdentifier {
    pub package: Package,
    pub name: String,
}

impl TypeIdentifier {
    /// Creates a TypeIdentifier for a type in the auto-imported `Global` package.
    pub fn global(name: &str) -> Self {
        Self {
            package: Package::Global,
            name: name.to_string(),
        }
    }

    /// Creates a TypeIdentifier with an explicit named package.
    pub fn new(package: &str, name: &str) -> Self {
        Self {
            package: Package::Named(package.to_string()),
            name: name.to_string(),
        }
    }

    /// Creates a TypeIdentifier with an unresolved package. All call sites
    /// will be updated in Phase 3 to use real packages.
    pub fn unresolved(name: &str) -> Self {
        Self {
            package: Package::Unresolved,
            name: name.to_string(),
        }
    }

    /// Same as [`Self::unresolved`] but takes an owned String to avoid cloning.
    pub fn unresolved_owned(name: String) -> Self {
        Self {
            package: Package::Unresolved,
            name,
        }
    }

    pub fn is_global(&self) -> bool {
        self.package == Package::Global
    }

    /// Returns a package-qualified name that is always unique across packages.
    /// Unlike `Display`, this prefixes `Global.` for stdlib types so they never
    /// collide with user-defined types of the same name.
    pub fn qualified_name(&self) -> String {
        match &self.package {
            Package::Global => format!("Global.{}", self.name),
            Package::Named(pkg) => format!("{pkg}.{}", self.name),
            Package::Unresolved => self.name.clone(),
        }
    }

    /// Parses a package-qualified string (as produced by [`Self::qualified_name`])
    /// back into a [`TypeIdentifier`]. Strings without a `.` separator fall back
    /// to [`Self::unresolved`], which the caller may prefer to resolve explicitly.
    pub fn from_qualified_name(qualified: &str) -> Self {
        match qualified.split_once('.') {
            Some(("Global", name)) => Self::global(name),
            Some((pkg, name)) if !pkg.is_empty() => Self::new(pkg, name),
            _ => Self::unresolved(qualified),
        }
    }
}

impl fmt::Display for TypeIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.package {
            Package::Global | Package::Unresolved => write!(f, "{}", self.name),
            Package::Named(pkg) => write!(f, "{pkg}.{}", self.name),
        }
    }
}
