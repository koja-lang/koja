use std::fmt;

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

/// Resolution attached to an AST reference site by typecheck.
///
/// Single-variant for resolved identifiers today (plus the `Unresolved`
/// in-flight state). Adding a future variant -- e.g. `Local(LocalId)` --
/// becomes a compiler-enforced migration thanks to exhaustiveness checks.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Resolution {
    #[default]
    Unresolved,
    Global(Identifier),
}

impl Resolution {
    pub fn is_resolved(&self) -> bool {
        matches!(self, Resolution::Global(_))
    }
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
