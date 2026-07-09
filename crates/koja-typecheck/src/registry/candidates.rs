//! Completion-candidate queries shared by the LSP and the REPL.
//!
//! Pure read-only walks over the registry: what members a type
//! exposes behind a dot, and what top-level symbols a package
//! declares. Consumers apply their own prefix filtering and
//! presentation (LSP `CompletionItem`s, shell replacement strings).

use koja_ast::identifier::{GlobalRegistryId, Resolution, ResolvedType};

use super::definitions::{Dispatch, FunctionSignature};
use super::{GlobalKind, GlobalRegistry, VisibilityScope};

/// Koja language keywords offered as completions.
pub const KEYWORDS: &[&str] = &[
    "break", "cond", "const", "else", "end", "enum", "extend", "false", "fn", "for", "if", "impl",
    "in", "loop", "match", "priv", "protocol", "receive", "return", "self", "spawn", "struct",
    "true", "type", "unless", "when", "while",
];

/// What kind of declaration or member a [`Candidate`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateKind {
    Constant,
    Enum,
    EnumVariant,
    Field,
    Function,
    Method,
    Protocol,
    Struct,
    TypeAlias,
}

/// Payload a consumer can render as a candidate's detail text.
/// Borrowed from the registry so no formatting happens up front.
#[derive(Clone, Copy, Debug)]
pub enum CandidateDetail<'a> {
    Function {
        signature: &'a FunctionSignature,
        type_params: &'a [String],
    },
    None,
    Type(&'a ResolvedType),
    TypeParams(&'a [String]),
}

/// One completion candidate: the completable name plus enough
/// registry-backed context to render kind and detail.
#[derive(Clone, Copy, Debug)]
pub struct Candidate<'a> {
    pub detail: CandidateDetail<'a>,
    pub kind: CandidateKind,
    pub label: &'a str,
}

impl GlobalRegistry {
    /// Members reachable behind a dot on `type_id`: methods whose
    /// dispatch matches `is_static`, plus fields for instance
    /// dispatch and enum variants for static dispatch.
    pub fn dot_candidates(&self, type_id: GlobalRegistryId, is_static: bool) -> Vec<Candidate<'_>> {
        let Some(owner) = self.get(type_id) else {
            return Vec::new();
        };
        let owner_path = owner.identifier.path();
        let mut candidates = Vec::new();
        for (_, entry) in self.iter_in_package(owner.identifier.package()) {
            let path = entry.identifier.path();
            if path.len() != owner_path.len() + 1 || path[..owner_path.len()] != *owner_path {
                continue;
            }
            let GlobalKind::Function(Some(signature)) = &entry.kind else {
                continue;
            };
            let dispatch_matches = match signature.dispatch {
                Dispatch::Instance => !is_static,
                Dispatch::Static => is_static,
            };
            if !dispatch_matches {
                continue;
            }
            candidates.push(Candidate {
                detail: CandidateDetail::Function {
                    signature,
                    type_params: &entry.type_params,
                },
                kind: CandidateKind::Method,
                label: path[owner_path.len()].as_str(),
            });
        }
        match &owner.kind {
            GlobalKind::Enum(Some(definition)) if is_static => {
                for variant in &definition.variants {
                    candidates.push(Candidate {
                        detail: CandidateDetail::None,
                        kind: CandidateKind::EnumVariant,
                        label: &variant.name,
                    });
                }
            }
            GlobalKind::Struct(Some(definition)) if !is_static => {
                for field in &definition.fields {
                    candidates.push(Candidate {
                        detail: CandidateDetail::Type(&field.ty),
                        kind: CandidateKind::Field,
                        label: &field.name,
                    });
                }
            }
            _ => {}
        }
        candidates
    }

    /// Top-level symbols declared in `pkg`, as visible from
    /// `caller_package` (package-private declarations are hidden from
    /// other packages). Instance methods and unstamped function
    /// signatures never appear.
    pub fn symbol_candidates<'a>(
        &'a self,
        pkg: &'a str,
        caller_package: &str,
    ) -> Vec<Candidate<'a>> {
        let mut candidates = Vec::new();
        for (_, entry) in self.iter_in_package(pkg) {
            let path = entry.identifier.path();
            if path.len() != 1 {
                continue;
            }
            if entry.visibility == VisibilityScope::PackagePrivate && pkg != caller_package {
                continue;
            }
            let label = path[0].as_str();
            let candidate = match &entry.kind {
                GlobalKind::Constant(Some(definition)) => Candidate {
                    detail: CandidateDetail::Type(&definition.ty),
                    kind: CandidateKind::Constant,
                    label,
                },
                GlobalKind::Constant(None) => Candidate {
                    detail: CandidateDetail::None,
                    kind: CandidateKind::Constant,
                    label,
                },
                GlobalKind::Enum(_) => Candidate {
                    detail: CandidateDetail::TypeParams(&entry.type_params),
                    kind: CandidateKind::Enum,
                    label,
                },
                GlobalKind::Function(Some(signature)) => {
                    if signature.params.iter().any(|p| p.name == "self") {
                        continue;
                    }
                    Candidate {
                        detail: CandidateDetail::Function {
                            signature,
                            type_params: &entry.type_params,
                        },
                        kind: CandidateKind::Function,
                        label,
                    }
                }
                GlobalKind::Function(None) => continue,
                GlobalKind::Protocol(_) => Candidate {
                    detail: CandidateDetail::TypeParams(&entry.type_params),
                    kind: CandidateKind::Protocol,
                    label,
                },
                GlobalKind::Struct(_) => Candidate {
                    detail: CandidateDetail::TypeParams(&entry.type_params),
                    kind: CandidateKind::Struct,
                    label,
                },
                GlobalKind::TypeAlias(Some(expansion)) => Candidate {
                    detail: CandidateDetail::Type(expansion),
                    kind: CandidateKind::TypeAlias,
                    label,
                },
                GlobalKind::TypeAlias(None) => Candidate {
                    detail: CandidateDetail::None,
                    kind: CandidateKind::TypeAlias,
                    label,
                },
            };
            candidates.push(candidate);
        }
        candidates
    }

    /// Walk a [`ResolvedType`] to its head [`Resolution::Global`] id,
    /// following type aliases. Unions use their first member
    /// (matching LSP hover behavior). `None` for anonymous types or
    /// unresolved heads.
    pub fn head_type_id(&self, ty: &ResolvedType) -> Option<GlobalRegistryId> {
        match ty {
            ResolvedType::Named {
                resolution: Resolution::Global(id),
                ..
            } => {
                if let Some(expansion) = self.alias_expansion(*id) {
                    return self.head_type_id(&expansion);
                }
                Some(*id)
            }
            ResolvedType::Union(members) => members.first().and_then(|m| self.head_type_id(m)),
            _ => None,
        }
    }
}
