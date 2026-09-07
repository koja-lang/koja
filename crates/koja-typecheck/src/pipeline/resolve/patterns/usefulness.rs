//! Pattern usefulness (Maranget, "Warnings for pattern matching").
//! One question drives both `match` checks. Given the rows already
//! in the matrix, can some value reach a new row? The match is
//! exhaustive when a wildcard row is not useful, and an arm is
//! unreachable when its own row is not useful against the arms above
//! it.
//!
//! Patterns are first deconstructed against the column type, so the
//! recursion only sees constructors, literals, wildcards and
//! or-alternatives. The column type supplies the constructor set,
//! which is the enum's variants, `true` / `false`, the union's
//! members, or the one constructor of a tuple or struct. Every other
//! type has infinitely many values and only a wildcard covers it.

use std::collections::BTreeSet;

use koja_ast::ast::{FieldPattern, Literal, Pattern};
use koja_ast::identifier::{AnonymousKind, GlobalRegistryId, Resolution, ResolvedType};

use super::super::types::{display_resolution, is_primitive, peel_alias, types_equivalent};
use super::literals::literal_repr;
use crate::pipeline::unify::{Substitution, substitute};
use crate::registry::{
    EnumDefinition, GlobalKind, GlobalRegistry, ResolvedStructField, ResolvedVariantData,
    StructDefinition,
};

/// One way to build a value of a finite column type.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Constructor {
    Bool(bool),
    /// The only constructor of a tuple or struct type.
    Single,
    /// Index into the canonical member list of a union.
    UnionMember(usize),
    /// Enum discriminant tag.
    Variant(u32),
}

/// A match pattern reduced to what usefulness needs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DeconstructedPattern {
    Constructor {
        ctor: Constructor,
        fields: Vec<DeconstructedPattern>,
    },
    /// A literal of an infinite type, keyed by its canonical text so
    /// duplicate literal arms compare equal. Never covers the column.
    Literal(String),
    /// A binary or list pattern. Matches some values, compares equal
    /// to nothing, never covers the column.
    Opaque,
    Or(Vec<DeconstructedPattern>),
    Wildcard,
}

const WILDCARD: DeconstructedPattern = DeconstructedPattern::Wildcard;

/// One row of the matrix, borrowed from the deconstructed arm
/// patterns so specialization never clones.
type Row<'a> = Vec<&'a DeconstructedPattern>;

/// Coverage questions over one `match` subject.
pub(crate) struct SubjectCoverage<'r> {
    registry: &'r GlobalRegistry,
    types: [ResolvedType; 1],
}

impl<'r> SubjectCoverage<'r> {
    pub(crate) fn new(subject_ty: &ResolvedType, registry: &'r GlobalRegistry) -> Self {
        Self {
            registry,
            types: [subject_ty.clone()],
        }
    }

    pub(crate) fn deconstruct(&self, pattern: &Pattern) -> Option<DeconstructedPattern> {
        deconstruct(pattern, &self.types[0], self.registry)
    }

    /// True when some value matches `candidate` and none of `earlier`.
    pub(crate) fn is_useful(
        &self,
        earlier: &[&DeconstructedPattern],
        candidate: &DeconstructedPattern,
    ) -> bool {
        !usefulness(
            &single_column(earlier),
            &[candidate],
            &self.types,
            self.registry,
        )
        .is_empty()
    }

    /// Rendered patterns for values no row in `rows` matches. Empty
    /// when the rows are exhaustive. A lone `_` means the subject
    /// type has no finite constructor set to enumerate.
    pub(crate) fn missing_patterns(&self, rows: &[&DeconstructedPattern]) -> Vec<String> {
        let mut rendered: Vec<String> = usefulness(
            &single_column(rows),
            &[&WILDCARD],
            &self.types,
            self.registry,
        )
        .iter()
        .map(|witness| render_witness(&witness[0], &self.types[0], self.registry))
        .collect();
        rendered.dedup();
        rendered
    }
}

fn single_column<'a>(rows: &[&'a DeconstructedPattern]) -> Vec<Row<'a>> {
    rows.iter().map(|pattern| vec![*pattern]).collect()
}

/// Every constructor of a column type, or `Infinite` when a wildcard
/// is the only pattern that can cover it.
enum ConstructorSet {
    Bool,
    Infinite,
    Single,
    Union(usize),
    Variants(usize),
}

impl ConstructorSet {
    fn all(&self) -> Option<Vec<Constructor>> {
        match self {
            ConstructorSet::Bool => Some(vec![Constructor::Bool(false), Constructor::Bool(true)]),
            ConstructorSet::Infinite => None,
            ConstructorSet::Single => Some(vec![Constructor::Single]),
            ConstructorSet::Union(count) => {
                Some((0..*count).map(Constructor::UnionMember).collect())
            }
            ConstructorSet::Variants(count) => {
                Some((0..*count as u32).map(Constructor::Variant).collect())
            }
        }
    }
}

fn constructor_set(ty: &ResolvedType, registry: &GlobalRegistry) -> ConstructorSet {
    let peeled = peel_alias(ty, registry);
    if is_primitive(&peeled, registry, "Bool") {
        return ConstructorSet::Bool;
    }
    match &peeled {
        ResolvedType::Union(members) => ConstructorSet::Union(members.len()),
        ResolvedType::Anonymous(AnonymousKind::Tuple { .. }) => ConstructorSet::Single,
        _ => {
            if let Some((_, definition, _)) = enum_of(&peeled, registry) {
                ConstructorSet::Variants(definition.variants.len())
            } else if struct_of(&peeled, registry).is_some() {
                ConstructorSet::Single
            } else {
                ConstructorSet::Infinite
            }
        }
    }
}

/// The registered enum behind a peeled `Named` type, with the type
/// args needed to view its payloads concretely.
fn enum_of<'r, 't>(
    ty: &'t ResolvedType,
    registry: &'r GlobalRegistry,
) -> Option<(GlobalRegistryId, &'r EnumDefinition, &'t [ResolvedType])> {
    let (id, type_args) = named_global(ty)?;
    let GlobalKind::Enum(Some(definition)) = &registry.get(id)?.kind else {
        return None;
    };
    Some((id, definition, type_args))
}

fn struct_of<'r, 't>(
    ty: &'t ResolvedType,
    registry: &'r GlobalRegistry,
) -> Option<(GlobalRegistryId, &'r StructDefinition, &'t [ResolvedType])> {
    let (id, type_args) = named_global(ty)?;
    let GlobalKind::Struct(Some(definition)) = &registry.get(id)?.kind else {
        return None;
    };
    Some((id, definition, type_args))
}

fn named_global(ty: &ResolvedType) -> Option<(GlobalRegistryId, &[ResolvedType])> {
    let ResolvedType::Named {
        resolution: Resolution::Global(id),
        type_args,
    } = ty
    else {
        return None;
    };
    Some((*id, type_args))
}

fn substituted(
    types: impl Iterator<Item = ResolvedType>,
    subst: &Substitution,
) -> Vec<ResolvedType> {
    types.map(|ty| substitute(&ty, subst)).collect()
}

/// Types of `ctor`'s payload columns when it builds a value of `ty`.
fn field_types(
    ctor: Constructor,
    ty: &ResolvedType,
    registry: &GlobalRegistry,
) -> Vec<ResolvedType> {
    let peeled = peel_alias(ty, registry);
    match ctor {
        Constructor::Bool(_) | Constructor::UnionMember(_) => Vec::new(),
        Constructor::Single => match &peeled {
            ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) => elements.clone(),
            _ => struct_of(&peeled, registry)
                .map(|(id, definition, type_args)| {
                    let subst = Substitution::from_args(id, type_args);
                    substituted(definition.fields.iter().map(|f| f.ty.clone()), &subst)
                })
                .unwrap_or_default(),
        },
        Constructor::Variant(tag) => enum_of(&peeled, registry)
            .and_then(|(id, definition, type_args)| {
                let variant = definition.variants.get(tag as usize)?;
                let subst = Substitution::from_args(id, type_args);
                Some(match &variant.data {
                    ResolvedVariantData::Struct(fields) => {
                        substituted(fields.iter().map(|f| f.ty.clone()), &subst)
                    }
                    ResolvedVariantData::Tuple(elements) => {
                        substituted(elements.iter().cloned(), &subst)
                    }
                    ResolvedVariantData::Unit => Vec::new(),
                })
            })
            .unwrap_or_default(),
    }
}

/// Reduce `pattern` against its column type. `None` means the shape
/// does not fit the type. Resolution already diagnosed that, so the
/// caller skips coverage rather than stacking a second error.
fn deconstruct(
    pattern: &Pattern,
    ty: &ResolvedType,
    registry: &GlobalRegistry,
) -> Option<DeconstructedPattern> {
    let peeled = peel_alias(ty, registry);
    match pattern {
        Pattern::Binary { .. } | Pattern::List { .. } => Some(DeconstructedPattern::Opaque),
        Pattern::Binding { .. } | Pattern::Wildcard { .. } => Some(WILDCARD),
        Pattern::Constructor { .. } => None,
        Pattern::EnumStruct {
            variant, fields, ..
        } => {
            let (tag, declared, subst) = variant_target(variant, &peeled, registry)?;
            let ResolvedVariantData::Struct(declared_fields) = declared else {
                return None;
            };
            let fields = deconstruct_fields(fields, declared_fields, &subst, registry)?;
            Some(constructor(Constructor::Variant(tag), fields))
        }
        Pattern::EnumTuple {
            variant, elements, ..
        } => {
            let (tag, declared, subst) = variant_target(variant, &peeled, registry)?;
            let ResolvedVariantData::Tuple(declared_elements) = declared else {
                return None;
            };
            if declared_elements.len() != elements.len() {
                return None;
            }
            let element_types = substituted(declared_elements.iter().cloned(), &subst);
            let fields = deconstruct_elements(elements, &element_types, registry)?;
            Some(constructor(Constructor::Variant(tag), fields))
        }
        Pattern::EnumUnit { variant, .. } => {
            let (tag, declared, _) = variant_target(variant, &peeled, registry)?;
            matches!(declared, ResolvedVariantData::Unit)
                .then(|| constructor(Constructor::Variant(tag), Vec::new()))
        }
        Pattern::Literal { value, .. } => Some(match value {
            Literal::Bool(flag) if is_primitive(&peeled, registry, "Bool") => {
                constructor(Constructor::Bool(*flag), Vec::new())
            }
            _ => DeconstructedPattern::Literal(literal_repr(value)),
        }),
        Pattern::Or { patterns, .. } => patterns
            .iter()
            .map(|alternative| deconstruct(alternative, ty, registry))
            .collect::<Option<Vec<_>>>()
            .map(DeconstructedPattern::Or),
        Pattern::Struct { fields, .. } => {
            let (id, definition, type_args) = struct_of(&peeled, registry)?;
            let subst = Substitution::from_args(id, type_args);
            let fields = deconstruct_fields(fields, &definition.fields, &subst, registry)?;
            Some(constructor(Constructor::Single, fields))
        }
        Pattern::Tuple { elements, .. } => {
            let ResolvedType::Anonymous(AnonymousKind::Tuple {
                elements: element_types,
            }) = &peeled
            else {
                return None;
            };
            if element_types.len() != elements.len() {
                return None;
            }
            let fields = deconstruct_elements(elements, element_types, registry)?;
            Some(constructor(Constructor::Single, fields))
        }
        Pattern::TypedBinding { resolved_type, .. } => {
            let bound = resolved_type.as_ref()?;
            let ResolvedType::Union(members) = &peeled else {
                return Some(WILDCARD);
            };
            let index = members
                .iter()
                .position(|member| types_equivalent(member, bound, registry))?;
            Some(constructor(Constructor::UnionMember(index), Vec::new()))
        }
    }
}

fn constructor(ctor: Constructor, fields: Vec<DeconstructedPattern>) -> DeconstructedPattern {
    DeconstructedPattern::Constructor { ctor, fields }
}

/// Tag, declared payload shape and type-arg substitution for
/// `variant_name` on the enum behind `peeled`.
fn variant_target<'r>(
    variant_name: &str,
    peeled: &ResolvedType,
    registry: &'r GlobalRegistry,
) -> Option<(u32, &'r ResolvedVariantData, Substitution)> {
    let (id, definition, type_args) = enum_of(peeled, registry)?;
    let (tag, variant) = definition.lookup_variant(variant_name)?;
    Some((tag, &variant.data, Substitution::from_args(id, type_args)))
}

fn deconstruct_elements(
    elements: &[Pattern],
    element_types: &[ResolvedType],
    registry: &GlobalRegistry,
) -> Option<Vec<DeconstructedPattern>> {
    elements
        .iter()
        .zip(element_types)
        .map(|(element, element_ty)| deconstruct(element, element_ty, registry))
        .collect()
}

/// Listed fields land in declared order. Omitted fields are
/// wildcards.
fn deconstruct_fields(
    listed: &[FieldPattern],
    declared: &[ResolvedStructField],
    subst: &Substitution,
    registry: &GlobalRegistry,
) -> Option<Vec<DeconstructedPattern>> {
    let mut fields = vec![WILDCARD; declared.len()];
    for field in listed {
        let index = declared.iter().position(|d| d.name == field.name)?;
        let field_ty = substitute(&declared[index].ty, subst);
        fields[index] = deconstruct(&field.pattern, &field_ty, registry)?;
    }
    Some(fields)
}

/// Witness rows for values that match `row` and none of `matrix`.
/// Empty when `row` is not useful.
fn usefulness(
    matrix: &[Row<'_>],
    row: &[&DeconstructedPattern],
    types: &[ResolvedType],
    registry: &GlobalRegistry,
) -> Vec<Vec<DeconstructedPattern>> {
    let Some((head, rest)) = row.split_first() else {
        return if matrix.is_empty() {
            vec![Vec::new()]
        } else {
            Vec::new()
        };
    };
    let (head_ty, rest_types) = types
        .split_first()
        .expect("usefulness row and type vector have equal length");
    let matrix = expand_or_heads(matrix);
    match head {
        DeconstructedPattern::Constructor { ctor, fields } => specialize_and_recurse(
            &matrix,
            *ctor,
            fields.iter().collect(),
            rest,
            head_ty,
            rest_types,
            registry,
        ),
        DeconstructedPattern::Literal(text) => {
            let same_literal = rows_with_head(&matrix, |head| {
                matches!(head, DeconstructedPattern::Wildcard)
                    || matches!(head, DeconstructedPattern::Literal(other) if other == text)
            });
            prefix_each(
                usefulness(&same_literal, rest, rest_types, registry),
                || DeconstructedPattern::Literal(text.clone()),
            )
        }
        DeconstructedPattern::Opaque => prefix_each(
            usefulness(&default_matrix(&matrix), rest, rest_types, registry),
            || DeconstructedPattern::Opaque,
        ),
        DeconstructedPattern::Or(alternatives) => alternatives
            .iter()
            .flat_map(|alternative| {
                let mut alternative_row = vec![alternative];
                alternative_row.extend_from_slice(rest);
                usefulness(&matrix, &alternative_row, types, registry)
            })
            .collect(),
        DeconstructedPattern::Wildcard => {
            wildcard_usefulness(&matrix, rest, head_ty, rest_types, registry)
        }
    }
}

/// A wildcard head splits on every constructor when the column's
/// heads form a complete signature. Otherwise it drops to the
/// default matrix and names a missing constructor (or `_` for an
/// infinite type) as the witness head.
fn wildcard_usefulness(
    matrix: &[Row<'_>],
    rest: &[&DeconstructedPattern],
    head_ty: &ResolvedType,
    rest_types: &[ResolvedType],
    registry: &GlobalRegistry,
) -> Vec<Vec<DeconstructedPattern>> {
    let present: BTreeSet<Constructor> = matrix
        .iter()
        .filter_map(|other| match other[0] {
            DeconstructedPattern::Constructor { ctor, .. } => Some(*ctor),
            _ => None,
        })
        .collect();
    let all = constructor_set(head_ty, registry).all();
    if let Some(all) = &all
        && all.iter().all(|ctor| present.contains(ctor))
    {
        return all
            .iter()
            .flat_map(|ctor| {
                let arity = field_types(*ctor, head_ty, registry).len();
                let head_fields = vec![&WILDCARD; arity];
                specialize_and_recurse(
                    matrix,
                    *ctor,
                    head_fields,
                    rest,
                    head_ty,
                    rest_types,
                    registry,
                )
            })
            .collect();
    }
    let tails = usefulness(&default_matrix(matrix), rest, rest_types, registry);
    let Some(all) = all else {
        return prefix_each(tails, || WILDCARD);
    };
    let missing: Vec<Constructor> = all
        .into_iter()
        .filter(|ctor| !present.contains(ctor))
        .collect();
    tails
        .into_iter()
        .flat_map(|tail| {
            missing.iter().map(move |ctor| {
                let arity = field_types(*ctor, head_ty, registry).len();
                let mut witness = vec![constructor(*ctor, vec![WILDCARD; arity])];
                witness.extend(tail.iter().cloned());
                witness
            })
        })
        .collect()
}

/// Keep the rows that can match `ctor`, replace the head column by
/// the constructor's field columns (`head_fields` for the tested
/// row), recurse, then fold the field columns of every witness back
/// into `ctor(...)`.
fn specialize_and_recurse(
    matrix: &[Row<'_>],
    ctor: Constructor,
    head_fields: Row<'_>,
    rest: &[&DeconstructedPattern],
    head_ty: &ResolvedType,
    rest_types: &[ResolvedType],
    registry: &GlobalRegistry,
) -> Vec<Vec<DeconstructedPattern>> {
    let arity = head_fields.len();
    let specialized: Vec<Row<'_>> = matrix
        .iter()
        .filter_map(|other| specialize_row(other, ctor, arity))
        .collect();
    let mut types = field_types(ctor, head_ty, registry);
    types.extend_from_slice(rest_types);
    let mut row = head_fields;
    row.extend_from_slice(rest);
    usefulness(&specialized, &row, &types, registry)
        .into_iter()
        .map(|mut witness| {
            let tail = witness.split_off(arity);
            let mut folded = vec![constructor(ctor, witness)];
            folded.extend(tail);
            folded
        })
        .collect()
}

fn specialize_row<'a>(row: &Row<'a>, ctor: Constructor, arity: usize) -> Option<Row<'a>> {
    let mut specialized: Row<'a> = match row[0] {
        DeconstructedPattern::Constructor {
            ctor: other,
            fields,
        } if *other == ctor => fields.iter().collect(),
        DeconstructedPattern::Wildcard => vec![&WILDCARD; arity],
        _ => return None,
    };
    specialized.extend_from_slice(&row[1..]);
    Some(specialized)
}

/// Rows whose head is a wildcard, with the head column dropped.
fn default_matrix<'a>(matrix: &[Row<'a>]) -> Vec<Row<'a>> {
    rows_with_head(matrix, |head| {
        matches!(head, DeconstructedPattern::Wildcard)
    })
}

fn rows_with_head<'a>(
    matrix: &[Row<'a>],
    keep: impl Fn(&DeconstructedPattern) -> bool,
) -> Vec<Row<'a>> {
    matrix
        .iter()
        .filter(|row| keep(row[0]))
        .map(|row| row[1..].to_vec())
        .collect()
}

/// Replace every row whose head is an or-pattern with one row per
/// alternative.
fn expand_or_heads<'a>(matrix: &[Row<'a>]) -> Vec<Row<'a>> {
    matrix
        .iter()
        .flat_map(|row| match row[0] {
            DeconstructedPattern::Or(alternatives) => alternatives
                .iter()
                .map(|alternative| {
                    let mut expanded = vec![alternative];
                    expanded.extend_from_slice(&row[1..]);
                    expanded
                })
                .collect(),
            _ => vec![row.clone()],
        })
        .collect()
}

fn prefix_each(
    tails: Vec<Vec<DeconstructedPattern>>,
    head: impl Fn() -> DeconstructedPattern,
) -> Vec<Vec<DeconstructedPattern>> {
    tails
        .into_iter()
        .map(|tail| {
            let mut witness = vec![head()];
            witness.extend(tail);
            witness
        })
        .collect()
}

/// Render a witness in pattern syntax, such as
/// `Option.Some(Color.Green)`, `(true, _)` or `Point{x: true}`.
fn render_witness(
    witness: &DeconstructedPattern,
    ty: &ResolvedType,
    registry: &GlobalRegistry,
) -> String {
    let peeled = peel_alias(ty, registry);
    let DeconstructedPattern::Constructor { ctor, fields } = witness else {
        return match witness {
            DeconstructedPattern::Literal(text) => text.clone(),
            _ => "_".to_string(),
        };
    };
    let field_types = field_types(*ctor, &peeled, registry);
    match ctor {
        Constructor::Bool(flag) => flag.to_string(),
        Constructor::Single => match &peeled {
            ResolvedType::Anonymous(AnonymousKind::Tuple { .. }) => {
                format!("({})", render_positional(fields, &field_types, registry))
            }
            _ => {
                let Some((id, definition, _)) = struct_of(&peeled, registry) else {
                    return "_".to_string();
                };
                let name = type_name(id, registry);
                let body = render_named(fields, &definition.fields, &field_types, registry);
                format!("{name}{{{body}}}")
            }
        },
        Constructor::UnionMember(index) => match &peeled {
            ResolvedType::Union(members) => members
                .get(*index)
                .map(|member| display_resolution(member, registry))
                .unwrap_or_else(|| "_".to_string()),
            _ => "_".to_string(),
        },
        Constructor::Variant(tag) => {
            let Some((id, definition, _)) = enum_of(&peeled, registry) else {
                return "_".to_string();
            };
            let Some(variant) = definition.variants.get(*tag as usize) else {
                return "_".to_string();
            };
            let head = format!("{}.{}", type_name(id, registry), variant.name);
            match &variant.data {
                ResolvedVariantData::Struct(declared) => {
                    let body = render_named(fields, declared, &field_types, registry);
                    format!("{head}{{{body}}}")
                }
                ResolvedVariantData::Tuple(_) => {
                    format!(
                        "{head}({})",
                        render_positional(fields, &field_types, registry)
                    )
                }
                ResolvedVariantData::Unit => head,
            }
        }
    }
}

fn type_name(id: GlobalRegistryId, registry: &GlobalRegistry) -> String {
    registry
        .get(id)
        .map(|entry| entry.identifier.last().to_string())
        .unwrap_or_else(|| "_".to_string())
}

fn render_positional(
    fields: &[DeconstructedPattern],
    field_types: &[ResolvedType],
    registry: &GlobalRegistry,
) -> String {
    fields
        .iter()
        .zip(field_types)
        .map(|(field, field_ty)| render_witness(field, field_ty, registry))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Named fields print only where the witness narrows. Omitted
/// fields are implicit wildcards in pattern syntax.
fn render_named(
    fields: &[DeconstructedPattern],
    declared: &[ResolvedStructField],
    field_types: &[ResolvedType],
    registry: &GlobalRegistry,
) -> String {
    fields
        .iter()
        .zip(declared)
        .zip(field_types)
        .filter(|((field, _), _)| !matches!(field, DeconstructedPattern::Wildcard))
        .map(|((field, declared_field), field_ty)| {
            format!(
                "{}: {}",
                declared_field.name,
                render_witness(field, field_ty, registry)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use koja_ast::identifier::{AnonymousKind, ResolvedType};

    use super::{Constructor, DeconstructedPattern, SubjectCoverage, WILDCARD};
    use crate::registry::GlobalRegistry;

    fn bool_pattern(flag: bool) -> DeconstructedPattern {
        DeconstructedPattern::Constructor {
            ctor: Constructor::Bool(flag),
            fields: Vec::new(),
        }
    }

    fn pair(first: DeconstructedPattern, second: DeconstructedPattern) -> DeconstructedPattern {
        DeconstructedPattern::Constructor {
            ctor: Constructor::Single,
            fields: vec![first, second],
        }
    }

    fn bool_pair_type(registry: &GlobalRegistry) -> ResolvedType {
        let bool_ty = registry.primitive("Bool");
        ResolvedType::Anonymous(AnonymousKind::Tuple {
            elements: vec![bool_ty.clone(), bool_ty],
        })
    }

    #[test]
    fn complete_signature_splits_and_exhausts() {
        let registry = GlobalRegistry::with_stdlib_stubs();
        let coverage = SubjectCoverage::new(&registry.primitive("Bool"), &registry);
        let rows = [bool_pattern(true), bool_pattern(false)];
        let rows: Vec<&DeconstructedPattern> = rows.iter().collect();
        assert!(coverage.missing_patterns(&rows).is_empty());
        assert!(!coverage.is_useful(&rows, &WILDCARD));
    }

    #[test]
    fn missing_constructor_is_the_witness() {
        let registry = GlobalRegistry::with_stdlib_stubs();
        let coverage = SubjectCoverage::new(&registry.primitive("Bool"), &registry);
        let only_true = bool_pattern(true);
        assert_eq!(coverage.missing_patterns(&[&only_true]), vec!["false"]);
    }

    #[test]
    fn infinite_type_falls_back_to_the_default_matrix() {
        let registry = GlobalRegistry::with_stdlib_stubs();
        let coverage = SubjectCoverage::new(&registry.primitive("Int"), &registry);
        let one = DeconstructedPattern::Literal("1".to_string());
        let two = DeconstructedPattern::Literal("2".to_string());
        assert_eq!(coverage.missing_patterns(&[&one, &two]), vec!["_"]);
        assert!(coverage.missing_patterns(&[&one, &WILDCARD]).is_empty());
        assert!(
            !coverage.is_useful(&[&one], &one),
            "duplicate literal is not useful"
        );
        assert!(coverage.is_useful(&[&one], &two));
    }

    #[test]
    fn tuple_columns_combine_and_render_positional_witnesses() {
        let registry = GlobalRegistry::with_stdlib_stubs();
        let coverage = SubjectCoverage::new(&bool_pair_type(&registry), &registry);
        let true_any = pair(bool_pattern(true), WILDCARD);
        let false_true = pair(bool_pattern(false), bool_pattern(true));
        assert_eq!(
            coverage.missing_patterns(&[&true_any, &false_true]),
            vec!["(false, false)"]
        );
        let false_false = pair(bool_pattern(false), bool_pattern(false));
        assert!(
            coverage
                .missing_patterns(&[&true_any, &false_true, &false_false])
                .is_empty()
        );
    }

    #[test]
    fn or_rows_expand_into_the_matrix() {
        let registry = GlobalRegistry::with_stdlib_stubs();
        let coverage = SubjectCoverage::new(&registry.primitive("Bool"), &registry);
        let both = DeconstructedPattern::Or(vec![bool_pattern(true), bool_pattern(false)]);
        assert!(coverage.missing_patterns(&[&both]).is_empty());
        assert!(!coverage.is_useful(&[&both], &bool_pattern(true)));
    }

    #[test]
    fn nested_wildcard_row_is_unreachable_after_full_split() {
        let registry = GlobalRegistry::with_stdlib_stubs();
        let coverage = SubjectCoverage::new(&bool_pair_type(&registry), &registry);
        let true_true = pair(bool_pattern(true), bool_pattern(true));
        let true_false = pair(bool_pattern(true), bool_pattern(false));
        let true_any = pair(bool_pattern(true), WILDCARD);
        assert!(!coverage.is_useful(&[&true_true, &true_false], &true_any));
        assert!(coverage.is_useful(&[&true_true], &true_any));
    }
}
