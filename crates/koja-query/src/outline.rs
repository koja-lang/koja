//! The declaration tree of one file, for an editor's outline view
//! and its symbol search.
//!
//! One [`Visitor`] walk builds the tree. Types, protocols, and
//! `impl` and `extend` blocks nest their members. Function bodies
//! declare nothing, so the walk does not enter them. Children come
//! out in source order whatever vectors the parser filed them in.

use koja_ast::ast::{
    BuiltinDecl, Constant, EnumDecl, File, Function, Item, Param, ProtocolDecl, ProtocolMethod,
    StructDecl, TestDecl, TypeAlias, TypeParam, Visibility,
};
use koja_ast::labels::type_expr_span;
use koja_ast::span::Span;
use koja_ast::visit::{self, Visitor};

use crate::display::type_expr_label;

/// One declaration and the declarations nested under it.
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineNode {
    pub children: Vec<OutlineNode>,
    /// A short qualifier for the outline row, such as a function's
    /// signature or a type's parameters. Private declarations carry
    /// a `priv` prefix.
    pub detail: Option<String>,
    pub kind: OutlineKind,
    pub name: String,
    /// The span to select when the user picks the node. The whole
    /// span for a node with no name of its own, such as a test.
    pub name_span: Span,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutlineKind {
    Builtin,
    Constant,
    Enum,
    EnumVariant,
    /// An `extend` block, named after its target.
    Extend,
    /// A function at the package level.
    Function,
    /// An `impl` block, named after its target.
    Impl,
    /// A function nested under a type or a block.
    Method,
    Protocol,
    ProtocolMethod,
    Struct,
    /// A `test` block, named by its description.
    Test,
    TypeAlias,
}

/// The declaration tree of `file`, in source order at every level.
/// A derived `impl` has no source of its own and is left out.
pub fn outline(file: &File) -> Vec<OutlineNode> {
    let mut builder = Builder {
        frames: vec![Vec::new()],
    };
    builder.visit_file(file);
    let mut nodes = builder.frames.pop().unwrap_or_default();
    sort(&mut nodes);
    nodes
}

/// The walk in progress. The last frame collects the children of
/// the node being built, and the first one the top level.
struct Builder {
    frames: Vec<Vec<OutlineNode>>,
}

impl Builder {
    fn push(&mut self, node: OutlineNode) {
        if let Some(frame) = self.frames.last_mut() {
            frame.push(node);
        }
    }

    /// Record `node` with the declarations `walk` finds as its
    /// children.
    fn nest(&mut self, mut node: OutlineNode, walk: impl FnOnce(&mut Self)) {
        self.frames.push(Vec::new());
        walk(self);
        node.children = self.frames.pop().unwrap_or_default();
        sort(&mut node.children);
        self.push(node);
    }

    fn in_type(&self) -> bool {
        self.frames.len() > 1
    }
}

impl<'ast> Visitor<'ast> for Builder {
    fn visit_item(&mut self, item: &'ast Item) {
        match item {
            Item::Alias(_) => {}
            Item::Builtin(decl) => {
                self.nest(builtin_node(decl), |b| visit::walk_item(b, item));
            }
            Item::Constant(constant) => self.push(constant_node(constant)),
            Item::Enum(decl) => {
                self.nest(enum_node(decl), |b| {
                    for variant in &decl.variants {
                        b.push(leaf(
                            OutlineKind::EnumVariant,
                            variant.name.text.clone(),
                            variant.span,
                            variant.span,
                        ));
                    }
                    visit::walk_item(b, item);
                });
            }
            Item::Extend(block) => {
                let node = OutlineNode {
                    children: Vec::new(),
                    detail: Some("extend".to_string()),
                    kind: OutlineKind::Extend,
                    name: type_expr_label(&block.target),
                    name_span: type_expr_span(&block.target),
                    span: block.span,
                };
                self.nest(node, |b| visit::walk_item(b, item));
            }
            Item::Function(function) => self.visit_function(function),
            Item::Impl(block) if block.span.synthetic => {}
            Item::Impl(block) => {
                let node = OutlineNode {
                    children: Vec::new(),
                    detail: Some(format!("impl {}", type_expr_label(&block.trait_expr))),
                    kind: OutlineKind::Impl,
                    name: type_expr_label(&block.target),
                    name_span: type_expr_span(&block.target),
                    span: block.span,
                };
                self.nest(node, |b| visit::walk_item(b, item));
            }
            Item::Protocol(decl) => {
                self.nest(protocol_node(decl), |b| visit::walk_item(b, item));
            }
            Item::Struct(decl) => {
                self.nest(struct_node(decl), |b| visit::walk_item(b, item));
            }
            Item::Test(test) => self.visit_test(test),
            Item::TypeAlias(alias) => self.push(type_alias_node(alias)),
        }
    }

    /// A body declares nothing, so the walk stops here.
    fn visit_function(&mut self, function: &'ast Function) {
        let kind = if self.in_type() {
            OutlineKind::Method
        } else {
            OutlineKind::Function
        };
        self.push(OutlineNode {
            children: Vec::new(),
            detail: with_visibility(function.visibility, Some(signature_detail(function))),
            kind,
            name: function.name.text.clone(),
            name_span: function.name.span,
            span: function.span,
        });
    }

    fn visit_protocol_method(&mut self, method: &'ast ProtocolMethod) {
        self.push(leaf(
            OutlineKind::ProtocolMethod,
            method.name.text.clone(),
            method.name.span,
            method.span,
        ));
    }

    fn visit_test(&mut self, test: &'ast TestDecl) {
        self.push(OutlineNode {
            children: Vec::new(),
            detail: Some("test".to_string()),
            kind: OutlineKind::Test,
            name: test.description.clone(),
            name_span: test.span,
            span: test.span,
        });
    }
}

fn leaf(kind: OutlineKind, name: String, name_span: Span, span: Span) -> OutlineNode {
    OutlineNode {
        children: Vec::new(),
        detail: None,
        kind,
        name,
        name_span,
        span,
    }
}

fn builtin_node(decl: &BuiltinDecl) -> OutlineNode {
    OutlineNode {
        children: Vec::new(),
        detail: type_params_detail(&decl.type_params),
        kind: OutlineKind::Builtin,
        name: decl.name().text.clone(),
        name_span: decl.name().span,
        span: decl.span,
    }
}

fn constant_node(constant: &Constant) -> OutlineNode {
    OutlineNode {
        children: Vec::new(),
        detail: with_visibility(constant.visibility, None),
        kind: OutlineKind::Constant,
        name: constant.name().text.clone(),
        name_span: constant.name().span,
        span: constant.span,
    }
}

fn enum_node(decl: &EnumDecl) -> OutlineNode {
    OutlineNode {
        children: Vec::new(),
        detail: with_visibility(decl.visibility, type_params_detail(&decl.type_params)),
        kind: OutlineKind::Enum,
        name: decl.name().text.clone(),
        name_span: decl.name().span,
        span: decl.span,
    }
}

fn protocol_node(decl: &ProtocolDecl) -> OutlineNode {
    OutlineNode {
        children: Vec::new(),
        detail: with_visibility(decl.visibility, type_params_detail(&decl.type_params)),
        kind: OutlineKind::Protocol,
        name: decl.name().text.clone(),
        name_span: decl.name().span,
        span: decl.span,
    }
}

fn struct_node(decl: &StructDecl) -> OutlineNode {
    OutlineNode {
        children: Vec::new(),
        detail: with_visibility(decl.visibility, type_params_detail(&decl.type_params)),
        kind: OutlineKind::Struct,
        name: decl.name().text.clone(),
        name_span: decl.name().span,
        span: decl.span,
    }
}

fn type_alias_node(alias: &TypeAlias) -> OutlineNode {
    OutlineNode {
        children: Vec::new(),
        detail: with_visibility(alias.visibility, Some(type_expr_label(&alias.type_expr))),
        kind: OutlineKind::TypeAlias,
        name: alias.name.text.clone(),
        name_span: alias.name.span,
        span: alias.span,
    }
}

/// `fn(name: Type, ...) -> Return` from the written signature.
fn signature_detail(function: &Function) -> String {
    let params: Vec<String> = function
        .params
        .iter()
        .map(|param| match param {
            Param::Self_ { .. } => "self".to_string(),
            Param::Regular {
                name, type_expr, ..
            } => format!("{}: {}", name, type_expr_label(type_expr)),
        })
        .collect();
    let ret = function
        .return_type
        .as_ref()
        .map(|t| format!(" -> {}", type_expr_label(t)))
        .unwrap_or_default();
    format!("fn({}){}", params.join(", "), ret)
}

/// `<T, U>`, or `None` when the list is empty.
fn type_params_detail(params: &[TypeParam]) -> Option<String> {
    if params.is_empty() {
        return None;
    }
    let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
    Some(format!("<{}>", names.join(", ")))
}

/// Prefix `detail` with `priv` for a private declaration.
fn with_visibility(visibility: Visibility, detail: Option<String>) -> Option<String> {
    if visibility == Visibility::Public {
        return detail;
    }
    Some(match detail {
        Some(detail) => format!("priv {detail}"),
        None => "priv".to_string(),
    })
}

fn sort(nodes: &mut [OutlineNode]) {
    nodes.sort_by_key(|node| (node.span.start.line, node.span.start.column));
}

#[cfg(test)]
mod tests {
    use koja_parser::{ParseMode, parse};

    use super::*;

    fn outline_of(source: &str) -> Vec<OutlineNode> {
        let parsed = parse(source, ParseMode::File);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        outline(&parsed.ast)
    }

    fn names(nodes: &[OutlineNode]) -> Vec<(&str, OutlineKind)> {
        nodes
            .iter()
            .map(|node| (node.name.as_str(), node.kind))
            .collect()
    }

    #[test]
    fn top_level_declarations_in_source_order() {
        let nodes = outline_of(
            "const LIMIT = 3\n\
             fn run() -> Int\n  1\nend\n\
             struct Point\n  x: Int\nend\n\
             enum Color\n  Red\nend\n\
             protocol Show\n  fn show(self) -> String\nend\n\
             type Alias = List<Int>\n",
        );
        assert_eq!(
            names(&nodes),
            vec![
                ("LIMIT", OutlineKind::Constant),
                ("run", OutlineKind::Function),
                ("Point", OutlineKind::Struct),
                ("Color", OutlineKind::Enum),
                ("Show", OutlineKind::Protocol),
                ("Alias", OutlineKind::TypeAlias),
            ]
        );
    }

    #[test]
    fn members_nest_under_their_type_in_source_order() {
        let nodes = outline_of(
            "struct Stack<T>\n\
             \x20 items: List<T>\n\
             \x20 fn push(self, item: T) -> Stack<T>\n    self\n  end\n\
             \x20 const EMPTY = 0\n\
             \x20 fn pop(self) -> T\n    self.items[0]\n  end\n\
             \x20 test \"pops\"\n    1\n  end\n\
             end\n",
        );
        assert_eq!(names(&nodes), vec![("Stack", OutlineKind::Struct)]);
        let stack = &nodes[0];
        assert_eq!(stack.detail.as_deref(), Some("<T>"));
        assert_eq!(
            names(&stack.children),
            vec![
                ("push", OutlineKind::Method),
                ("EMPTY", OutlineKind::Constant),
                ("pop", OutlineKind::Method),
                ("pops", OutlineKind::Test),
            ]
        );
        assert_eq!(
            stack.children[0].detail.as_deref(),
            Some("fn(self, item: T) -> Stack<T>")
        );
        assert_eq!(stack.children[3].name_span, stack.children[3].span);
    }

    #[test]
    fn enum_variants_and_protocol_methods_are_leaves() {
        let nodes = outline_of(
            "enum Shape\n  Circle(Float)\n  Square\nend\n\
             protocol Area\n  fn area(self) -> Float\nend\n",
        );
        assert_eq!(
            names(&nodes[0].children),
            vec![
                ("Circle", OutlineKind::EnumVariant),
                ("Square", OutlineKind::EnumVariant),
            ]
        );
        assert_eq!(
            names(&nodes[1].children),
            vec![("area", OutlineKind::ProtocolMethod)]
        );
    }

    #[test]
    fn impl_and_extend_blocks_name_their_target() {
        let nodes = outline_of(
            "struct Point\n  x: Int\nend\n\
             impl Display for Point\n  fn to_string(self) -> String\n    \"p\"\n  end\nend\n\
             extend Point\n  fn double(self) -> Point\n    self\n  end\nend\n",
        );
        let imp = &nodes[1];
        assert_eq!((imp.name.as_str(), imp.kind), ("Point", OutlineKind::Impl));
        assert_eq!(imp.detail.as_deref(), Some("impl Display"));
        assert_eq!(
            names(&imp.children),
            vec![("to_string", OutlineKind::Method)]
        );
        let ext = &nodes[2];
        assert_eq!(
            (ext.name.as_str(), ext.kind),
            ("Point", OutlineKind::Extend)
        );
        assert_eq!(ext.detail.as_deref(), Some("extend"));
        assert_eq!(names(&ext.children), vec![("double", OutlineKind::Method)]);
    }

    #[test]
    fn private_declarations_carry_a_priv_prefix() {
        let nodes = outline_of("priv fn helper() -> Int\n  1\nend\npriv const K = 1\n");
        assert_eq!(nodes[0].detail.as_deref(), Some("priv fn() -> Int"));
        assert_eq!(nodes[1].detail.as_deref(), Some("priv"));
    }
}
