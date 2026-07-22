//! Cycle-breaking pass: wraps recursive struct fields / enum payload
//! slots with [`IRType::Indirect`] so backends never face a value-
//! level recursive type. Mirrors v1's `mark_recursive_fields` over
//! the post-monomorphization IR graph.
//!
//! Edges count inline `Struct(_)` / `Enum(_)` references only, since
//! pointer-shaped types (`CPtr`, `List`, `Map`, `Set`, `Function`,
//! existing `Indirect`) already break the size dependency. Back-
//! edges discovered by a three-color DFS pick out the source slot,
//! and the second walk rewrites that slot's `IRType` in place.

use std::collections::{BTreeMap, BTreeSet};

use crate::enum_decl::{IREnumDecl, IRVariantPayload};
use crate::function::IRSymbol;
use crate::package::IRPackage;
use crate::struct_decl::IRStructDecl;
use crate::types::IRType;

/// Walk every (struct, enum) decl across `packages`, identify back-
/// edges in the inline-reference graph, and rewrite each recorded
/// slot's [`IRType`] with [`IRType::Indirect`]. Idempotent on a
/// cycle-free IR.
pub(crate) fn break_type_cycles(packages: &mut [IRPackage]) {
    let graph = build_graph(packages);
    let recursive_slots = find_back_edge_slots(&graph);
    if recursive_slots.is_empty() {
        return;
    }
    apply_indirect_rewrites(packages, &recursive_slots);
}

/// Slot inside a struct / enum decl that a back-edge discovery
/// pinned: the offending field / payload position.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Slot {
    StructField(u32),
    EnumTupleElement { variant: u32, element: u32 },
    EnumStructField { variant: u32, field_index: u32 },
}

#[derive(Clone, Debug)]
struct Edge {
    target: IRSymbol,
    slot: Slot,
}

fn build_graph(packages: &[IRPackage]) -> BTreeMap<IRSymbol, Vec<Edge>> {
    let mut graph: BTreeMap<IRSymbol, Vec<Edge>> = BTreeMap::new();
    for package in packages {
        for decl in package.structs.values() {
            graph.insert(decl.symbol.clone(), collect_struct_edges(decl));
        }
        for decl in package.enums.values() {
            graph.insert(decl.symbol.clone(), collect_enum_edges(decl));
        }
    }
    graph
}

fn collect_struct_edges(decl: &IRStructDecl) -> Vec<Edge> {
    let mut edges = Vec::new();
    for field in &decl.fields {
        for target in inline_refs(&field.ir_type) {
            edges.push(Edge {
                target,
                slot: Slot::StructField(field.index),
            });
        }
    }
    edges
}

fn collect_enum_edges(decl: &IREnumDecl) -> Vec<Edge> {
    let mut edges = Vec::new();
    for (variant_idx, variant) in decl.variants.iter().enumerate() {
        let variant_idx = variant_idx as u32;
        match &variant.payload {
            IRVariantPayload::Tuple(types) => {
                for (element_idx, ty) in types.iter().enumerate() {
                    for target in inline_refs(ty) {
                        edges.push(Edge {
                            target,
                            slot: Slot::EnumTupleElement {
                                variant: variant_idx,
                                element: element_idx as u32,
                            },
                        });
                    }
                }
            }
            IRVariantPayload::Struct(fields) => {
                for field in fields {
                    for target in inline_refs(&field.ir_type) {
                        edges.push(Edge {
                            target,
                            slot: Slot::EnumStructField {
                                variant: variant_idx,
                                field_index: field.index,
                            },
                        });
                    }
                }
            }
            IRVariantPayload::Unit => {}
        }
    }
    edges
}

/// Symbols of every struct / enum that contribute inline (non-pointer)
/// size to `ty`'s value layout. Pointer-shaped wrappers stop the
/// recursion, as they already break the size cycle for the LLVM layer.
fn inline_refs(ty: &IRType) -> Vec<IRSymbol> {
    match ty {
        IRType::Struct(symbol) | IRType::Enum(symbol) => vec![symbol.clone()],
        IRType::Tuple(elements) => elements.iter().flat_map(inline_refs).collect(),
        IRType::Union { members, .. } => members.iter().flat_map(inline_refs).collect(),
        IRType::Binary
        | IRType::Bits
        | IRType::Bool
        | IRType::CPtr(_)
        | IRType::Float32
        | IRType::Float64
        | IRType::Function { .. }
        | IRType::Indirect(_)
        | IRType::Int8
        | IRType::Int16
        | IRType::Int32
        | IRType::Int64
        | IRType::List(_)
        | IRType::Map { .. }
        | IRType::Set(_)
        | IRType::String
        | IRType::UInt8
        | IRType::UInt16
        | IRType::UInt32
        | IRType::UInt64
        | IRType::Unit => Vec::new(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Color {
    White,
    Gray,
    Black,
}

fn find_back_edge_slots(
    graph: &BTreeMap<IRSymbol, Vec<Edge>>,
) -> BTreeMap<IRSymbol, BTreeSet<Slot>> {
    let mut colors: BTreeMap<IRSymbol, Color> =
        graph.keys().map(|s| (s.clone(), Color::White)).collect();
    let mut hits: BTreeMap<IRSymbol, BTreeSet<Slot>> = BTreeMap::new();
    for node in graph.keys() {
        if colors[node] == Color::White {
            dfs(node, graph, &mut colors, &mut hits);
        }
    }
    hits
}

fn dfs(
    node: &IRSymbol,
    graph: &BTreeMap<IRSymbol, Vec<Edge>>,
    colors: &mut BTreeMap<IRSymbol, Color>,
    hits: &mut BTreeMap<IRSymbol, BTreeSet<Slot>>,
) {
    colors.insert(node.clone(), Color::Gray);
    if let Some(edges) = graph.get(node) {
        for edge in edges {
            match colors.get(&edge.target).copied() {
                Some(Color::Gray) => {
                    hits.entry(node.clone())
                        .or_default()
                        .insert(edge.slot.clone());
                }
                Some(Color::White) => dfs(&edge.target, graph, colors, hits),
                _ => {}
            }
        }
    }
    colors.insert(node.clone(), Color::Black);
}

fn apply_indirect_rewrites(
    packages: &mut [IRPackage],
    rewrites: &BTreeMap<IRSymbol, BTreeSet<Slot>>,
) {
    for package in packages {
        for decl in package.structs.values_mut() {
            if let Some(slots) = rewrites.get(&decl.symbol) {
                rewrite_struct(decl, slots);
            }
        }
        for decl in package.enums.values_mut() {
            if let Some(slots) = rewrites.get(&decl.symbol) {
                rewrite_enum(decl, slots);
            }
        }
    }
}

fn rewrite_struct(decl: &mut IRStructDecl, slots: &BTreeSet<Slot>) {
    for slot in slots {
        let Slot::StructField(index) = slot else {
            continue;
        };
        if let Some(field) = decl.fields.iter_mut().find(|f| f.index == *index) {
            wrap_indirect(&mut field.ir_type);
        }
    }
}

fn rewrite_enum(decl: &mut IREnumDecl, slots: &BTreeSet<Slot>) {
    for slot in slots {
        let (variant_idx, target) = match slot {
            Slot::EnumTupleElement { variant, element } => (variant, Some(*element)),
            Slot::EnumStructField {
                variant,
                field_index,
            } => (variant, Some(*field_index)),
            Slot::StructField(_) => continue,
        };
        let Some(variant) = decl.variants.get_mut(*variant_idx as usize) else {
            continue;
        };
        match (&mut variant.payload, target, slot) {
            (IRVariantPayload::Tuple(types), Some(idx), Slot::EnumTupleElement { .. }) => {
                if let Some(ty) = types.get_mut(idx as usize) {
                    wrap_indirect(ty);
                }
            }
            (IRVariantPayload::Struct(fields), Some(idx), Slot::EnumStructField { .. }) => {
                if let Some(field) = fields.iter_mut().find(|f| f.index == idx) {
                    wrap_indirect(&mut field.ir_type);
                }
            }
            _ => {}
        }
    }
}

fn wrap_indirect(ty: &mut IRType) {
    if matches!(ty, IRType::Indirect(_)) {
        return;
    }
    let inner = std::mem::replace(ty, IRType::Unit);
    *ty = IRType::Indirect(Box::new(inner));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enum_decl::{IREnumVariant, IRVariantTag};
    use crate::struct_decl::IRStructField;
    use koja_ast::identifier::Identifier;
    use std::collections::BTreeMap;

    fn sym(name: &str) -> IRSymbol {
        IRSymbol::from_identifier(&Identifier::new("TestApp", vec![name.to_string()]))
    }

    fn make_package(structs: Vec<IRStructDecl>, enums: Vec<IREnumDecl>) -> IRPackage {
        let mut struct_map = BTreeMap::new();
        for decl in structs {
            struct_map.insert(decl.symbol.clone(), decl);
        }
        let mut enum_map = BTreeMap::new();
        for decl in enums {
            enum_map.insert(decl.symbol.clone(), decl);
        }
        IRPackage {
            constants: BTreeMap::new(),
            enums: enum_map,
            functions: BTreeMap::new(),
            package: "TestApp".to_string(),
            structs: struct_map,
            unions: BTreeMap::new(),
        }
    }

    #[test]
    fn breaks_self_referential_struct_field() {
        let node = sym("Node");
        let decl = IRStructDecl {
            symbol: node.clone(),
            fields: vec![
                IRStructField {
                    index: 0,
                    ir_type: IRType::Int64,
                    name: "value".to_string(),
                },
                IRStructField {
                    index: 1,
                    ir_type: IRType::Struct(node.clone()),
                    name: "next".to_string(),
                },
            ],
        };
        let mut packages = vec![make_package(vec![decl], Vec::new())];
        break_type_cycles(&mut packages);
        let rewritten = &packages[0].structs[&node];
        assert_eq!(rewritten.fields[0].ir_type, IRType::Int64);
        assert!(matches!(
            rewritten.fields[1].ir_type,
            IRType::Indirect(ref inner) if **inner == IRType::Struct(node.clone())
        ));
    }

    #[test]
    fn breaks_self_referential_enum_tuple_variant() {
        let tree = sym("Tree");
        let decl = IREnumDecl {
            symbol: tree.clone(),
            variants: vec![
                IREnumVariant {
                    name: "Leaf".to_string(),
                    payload: IRVariantPayload::Tuple(vec![IRType::Int64]),
                    tag: IRVariantTag(0),
                },
                IREnumVariant {
                    name: "Branch".to_string(),
                    payload: IRVariantPayload::Tuple(vec![
                        IRType::Enum(tree.clone()),
                        IRType::Enum(tree.clone()),
                    ]),
                    tag: IRVariantTag(1),
                },
            ],
        };
        let mut packages = vec![make_package(Vec::new(), vec![decl])];
        break_type_cycles(&mut packages);
        let rewritten = &packages[0].enums[&tree];
        let IRVariantPayload::Tuple(branch_types) = &rewritten.variants[1].payload else {
            panic!("expected tuple payload");
        };
        for ty in branch_types {
            assert!(matches!(
                ty,
                IRType::Indirect(inner) if **inner == IRType::Enum(tree.clone())
            ));
        }
    }

    #[test]
    fn breaks_mutual_recursion_at_back_edge() {
        // Either Node.next OR Option.Some.tuple[0] gets the
        // indirection. Both cut the cycle, and the exact slot
        // depends on which decl the DFS hits first.
        let node = sym("Node");
        let option_node = IRSymbol::from_identifier(&Identifier::new(
            "Global",
            vec!["Option_$TestApp.Node$".to_string()],
        ));
        let node_decl = IRStructDecl {
            symbol: node.clone(),
            fields: vec![
                IRStructField {
                    index: 0,
                    ir_type: IRType::Int64,
                    name: "value".to_string(),
                },
                IRStructField {
                    index: 1,
                    ir_type: IRType::Enum(option_node.clone()),
                    name: "next".to_string(),
                },
            ],
        };
        let option_decl = IREnumDecl {
            symbol: option_node.clone(),
            variants: vec![
                IREnumVariant {
                    name: "Some".to_string(),
                    payload: IRVariantPayload::Tuple(vec![IRType::Struct(node.clone())]),
                    tag: IRVariantTag(0),
                },
                IREnumVariant {
                    name: "None".to_string(),
                    payload: IRVariantPayload::Unit,
                    tag: IRVariantTag(1),
                },
            ],
        };
        let mut packages = vec![make_package(vec![node_decl], vec![option_decl])];
        break_type_cycles(&mut packages);
        let rewritten_option = &packages[0].enums[&option_node];
        let IRVariantPayload::Tuple(some_types) = &rewritten_option.variants[0].payload else {
            panic!("expected tuple payload");
        };
        let option_cut = matches!(
            &some_types[0],
            IRType::Indirect(inner) if **inner == IRType::Struct(node.clone())
        );
        let node_cut = matches!(
            &packages[0].structs[&node].fields[1].ir_type,
            IRType::Indirect(inner) if **inner == IRType::Enum(option_node.clone())
        );
        assert!(
            option_cut ^ node_cut,
            "exactly one back-edge slot should land on Indirect; \
             option_cut={option_cut} node_cut={node_cut}",
        );
    }

    #[test]
    fn leaves_acyclic_decls_untouched() {
        let pair = sym("Pair");
        let decl = IRStructDecl {
            symbol: pair.clone(),
            fields: vec![
                IRStructField {
                    index: 0,
                    ir_type: IRType::Int64,
                    name: "first".to_string(),
                },
                IRStructField {
                    index: 1,
                    ir_type: IRType::String,
                    name: "second".to_string(),
                },
            ],
        };
        let mut packages = vec![make_package(vec![decl.clone()], Vec::new())];
        break_type_cycles(&mut packages);
        assert_eq!(packages[0].structs[&pair].fields, decl.fields);
    }
}
