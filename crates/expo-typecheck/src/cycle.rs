//! Cycle detection for recursive struct and enum types. Identifies
//! self-referential field types via DFS back-edge detection and wraps them in
//! [`Type::Indirect`] so that codegen emits heap-allocated pointers instead of
//! inlining them (which would produce infinite-size layouts).

use std::collections::{HashMap, HashSet};

use crate::context::{TypeContext, TypeKind, VariantData};
use crate::types::Type;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Color {
    White,
    Gray,
    Black,
}

#[derive(Clone, Hash, PartialEq, Eq)]
enum Slot {
    StructField(usize),
    EnumElement(usize, usize),
}

struct Edge {
    target: String,
    slot: Slot,
}

/// Detects recursive struct/enum field types in `ctx` and wraps them in
/// [`Type::Indirect`] for heap-allocated indirection.
pub fn mark_recursive_fields(ctx: &mut TypeContext) {
    let all_type_names: HashSet<String> = ctx
        .types
        .iter()
        .filter(|(_, ti)| ti.is_struct() || ti.is_enum())
        .map(|(n, _)| n.clone())
        .collect();

    let mut graph: HashMap<String, Vec<Edge>> = HashMap::new();

    for (name, ti) in &ctx.types {
        match &ti.kind {
            TypeKind::Struct { fields } => {
                let mut edges = Vec::new();
                for (idx, (_, field_ty)) in fields.iter().enumerate() {
                    for target in referenced_type_names(field_ty, &all_type_names) {
                        edges.push(Edge {
                            target,
                            slot: Slot::StructField(idx),
                        });
                    }
                }
                graph.insert(name.clone(), edges);
            }
            TypeKind::Enum { variants } => {
                let mut edges = Vec::new();
                for (vi, variant) in variants.iter().enumerate() {
                    let types: Vec<&Type> = match &variant.data {
                        VariantData::Tuple(types) => types.iter().collect(),
                        VariantData::Struct(fields) => fields.iter().map(|(_, ty)| ty).collect(),
                        VariantData::Unit => Vec::new(),
                    };
                    for (ei, ty) in types.iter().enumerate() {
                        for target in referenced_type_names(ty, &all_type_names) {
                            edges.push(Edge {
                                target,
                                slot: Slot::EnumElement(vi, ei),
                            });
                        }
                    }
                }
                graph.insert(name.clone(), edges);
            }
            TypeKind::Primitive => {}
        }
    }

    let mut colors: HashMap<String, Color> =
        graph.keys().map(|n| (n.clone(), Color::White)).collect();
    let mut wrap_slots: HashMap<String, HashSet<Slot>> = HashMap::new();

    let mut sorted: Vec<String> = graph.keys().cloned().collect();
    sorted.sort();

    for name in &sorted {
        if colors[name] == Color::White {
            dfs(name, &graph, &mut colors, &mut wrap_slots);
        }
    }

    for (type_name, slots) in &wrap_slots {
        if let Some(ti) = ctx.types.get_mut(type_name) {
            for slot in slots {
                match (&mut ti.kind, slot) {
                    (TypeKind::Struct { fields }, Slot::StructField(idx))
                        if *idx < fields.len() =>
                    {
                        let (fname, fty) = fields[*idx].clone();
                        fields[*idx] = (fname, Type::Indirect(Box::new(fty)));
                    }
                    (TypeKind::Enum { variants }, Slot::EnumElement(vi, ei))
                        if *vi < variants.len() =>
                    {
                        let variant = &mut variants[*vi];
                        match &mut variant.data {
                            VariantData::Tuple(types) if *ei < types.len() => {
                                types[*ei] = Type::Indirect(Box::new(types[*ei].clone()));
                            }
                            VariantData::Struct(fields) if *ei < fields.len() => {
                                let (fname, fty) = fields[*ei].clone();
                                fields[*ei] = (fname, Type::Indirect(Box::new(fty)));
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Three-color DFS that detects back edges (cycles). When a back edge is found,
/// the source node's field/variant slot is recorded in `wrap_slots` so it can
/// be wrapped in [`Type::Indirect`] later.
fn dfs(
    node: &str,
    graph: &HashMap<String, Vec<Edge>>,
    colors: &mut HashMap<String, Color>,
    wrap_slots: &mut HashMap<String, HashSet<Slot>>,
) {
    colors.insert(node.to_string(), Color::Gray);

    if let Some(edges) = graph.get(node) {
        for edge in edges {
            match colors.get(&edge.target) {
                Some(Color::Gray) => {
                    wrap_slots
                        .entry(node.to_string())
                        .or_default()
                        .insert(edge.slot.clone());
                }
                Some(Color::White) => {
                    dfs(&edge.target, graph, colors, wrap_slots);
                }
                _ => {}
            }
        }
    }

    colors.insert(node.to_string(), Color::Black);
}

/// Extracts all struct/enum names referenced by a type, filtering to only
/// names present in `known`.
fn referenced_type_names(ty: &Type, known: &HashSet<String>) -> HashSet<String> {
    let mut refs = HashSet::new();
    collect_refs(ty, known, &mut refs);
    refs
}

/// Recursively collects type names from `ty` that appear in `known`, appending to `refs`.
fn collect_refs(ty: &Type, known: &HashSet<String>, refs: &mut HashSet<String>) {
    match ty {
        Type::Struct(name) | Type::Enum(name) => {
            if known.contains(name) {
                refs.insert(name.clone());
            }
        }
        Type::GenericInstance {
            base, type_args, ..
        } => {
            if known.contains(base) {
                refs.insert(base.clone());
            }
            for arg in type_args {
                collect_refs(arg, known, refs);
            }
        }
        Type::Indirect(inner) => collect_refs(inner, known, refs),
        Type::Union(members) => {
            for m in members {
                collect_refs(m, known, refs);
            }
        }
        Type::Function {
            params,
            return_type,
        } => {
            for fp in params {
                collect_refs(&fp.ty, known, refs);
            }
            collect_refs(return_type, known, refs);
        }
        _ => {}
    }
}
