use std::fmt;

/// Which package a type belongs to. Used by [`TypeIdentifier`] to distinguish
/// types with the same name from different packages.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Package {
    /// The built-in standard library (auto-imported).
    Std,
    /// A named package (e.g. `json`, `net`, or the user's project name).
    Named(String),
    /// Package not yet determined. Present only during early pipeline stages;
    /// resolved to a concrete package before codegen.
    Unresolved,
}

impl Package {
    /// Builds a package-qualified key (e.g. `"std.List"` or `"alpha.Config"`)
    /// suitable for the `name_index` reverse lookup. Returns `None` for
    /// [`Package::Unresolved`] since unresolved packages have no scope to
    /// qualify against.
    pub fn qualify(&self, name: &str) -> Option<String> {
        match self {
            Package::Std => Some(format!("std.{name}")),
            Package::Named(p) => Some(format!("{p}.{name}")),
            Package::Unresolved => None,
        }
    }
}

impl fmt::Display for Package {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Package::Std => write!(f, "std"),
            Package::Named(name) => write!(f, "{name}"),
            Package::Unresolved => Ok(()),
        }
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
    /// Creates a TypeIdentifier for a type in the `std` package.
    pub fn std(name: &str) -> Self {
        Self {
            package: Package::Std,
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

    pub fn is_std(&self) -> bool {
        self.package == Package::Std
    }

    /// Returns a package-qualified name that is always unique across packages.
    /// Unlike `Display`, this prefixes `std.` for stdlib types so they never
    /// collide with user-defined types of the same name.
    pub fn qualified_name(&self) -> String {
        match &self.package {
            Package::Std => format!("std.{}", self.name),
            Package::Named(pkg) => format!("{pkg}.{}", self.name),
            Package::Unresolved => self.name.clone(),
        }
    }

    /// Parses a package-qualified string (as produced by [`Self::qualified_name`])
    /// back into a [`TypeIdentifier`]. Strings without a `.` separator fall back
    /// to [`Self::unresolved`], which the caller may prefer to resolve explicitly.
    pub fn from_qualified_name(qualified: &str) -> Self {
        match qualified.split_once('.') {
            Some(("std", name)) => Self::std(name),
            Some((pkg, name)) if !pkg.is_empty() => Self::new(pkg, name),
            _ => Self::unresolved(qualified),
        }
    }
}

impl fmt::Display for TypeIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.package {
            Package::Std | Package::Unresolved => write!(f, "{}", self.name),
            Package::Named(pkg) => write!(f, "{pkg}.{}", self.name),
        }
    }
}
