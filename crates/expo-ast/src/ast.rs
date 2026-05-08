//! The Expo abstract syntax tree.
//!
//! A single source file parses into a [`File`], which contains a list of
//! top-level [`Item`]s (functions, structs, enums, imports, constants, impls).
//! Functions hold a body of [`Statement`]s, which in turn contain [`Expr`]
//! nodes. [`Pattern`]s appear in `match` arms, `for` loops, and destructuring
//! assignments.

use std::path::PathBuf;

use crate::identifier::{LocalId, Resolution, ResolvedType, TypeIdentifier};
use crate::span::Span;
use crate::types::Type;

// Semantic enums

/// How a value crosses a scope boundary: parameter passing, closure capture,
/// or message send.
///
/// In the parser, `Move` is produced when the `move` keyword is present;
/// `Borrow` is the default for all other parameters and receivers.
/// `Copy` is resolved during type checking for closure captures of copy types.
///
/// ```expo
/// fn update(move self, name: String) -> User  # self is Move, name is Borrow
///   ...
/// end
///
/// multiplier = 3
/// triple = fn (x: Int32) -> Int32
///   x * multiplier                             # multiplier captured as Copy
/// end
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassMode {
    /// Value is duplicated; the original stays live.
    Copy,
    /// Ownership transfers; the original is consumed.
    Move,
    /// Read-only reference; the original stays live and accessible.
    Borrow,
}

/// Visibility marker on functions: `Public` (default) or `Private` (from the
/// `priv` keyword). The enforcement scope of `Private` depends on where the
/// function is declared -- type-internal for impl methods, file-private for
/// top-level functions.
///
/// ```expo
/// fn public_function         # Visibility::Public (the default)
///   ...
/// end
///
/// priv fn internal_helper    # Visibility::Private
///   ...
/// end
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Callable from anywhere the function can be named.
    Public,
    /// Callable only from within the function's declaration scope.
    Private,
}

// Top level

/// The value attached to an annotation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnnotationValue {
    /// A string value: `@doc "text"` or `@doc """text"""`.
    String(String),
    /// An explicit false: `@doc false` — suppresses documentation.
    False,
}

/// A metadata annotation such as `@doc` or `@extern`.
///
/// The struct is the verbatim source-shape (raw `name` + optional
/// `value`); semantic classification flows through [`Annotation::kind`],
/// which folds the recognized vocabulary into the structured
/// [`AnnotationKind`] enum. Tools that only care about source shape
/// (formatter, doc extractor) keep reading `name`/`value` directly;
/// anything that wants exhaustive case analysis reaches for
/// [`Annotation::kind`].
#[derive(Debug, Clone)]
pub struct Annotation {
    pub name: String,
    pub value: Option<AnnotationValue>,
    pub span: Span,
}

/// Payload variants for a well-formed `@doc` annotation. Mirrors the
/// two source shapes that have semantic meaning today; bare `@doc`
/// (no value) is **not** a `DocAttr` — it falls through to
/// [`AnnotationKind::Unknown`] because no consumer treats it as
/// documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocAttr {
    /// `@doc "text"` — the docstring payload tools render in
    /// `expo-doc` / LSP hovers.
    Text(String),
    /// `@doc false` — explicit "do not document this declaration"
    /// marker. Used by `expo-doc` to elide the decl from generated
    /// output.
    Suppressed,
}

/// Structured classification of an [`Annotation`], borrowed from the
/// underlying `name` and `value` fields so call sites pay no
/// allocation cost for inspecting an annotation's kind.
///
/// Every variant matches a single source-shape recognized somewhere
/// in the compiler. Malformed shapes (e.g. `@extern false`,
/// `@link 42`, bare `@intrinsic "foo"`) and unrecognized names
/// (anything not in the known vocabulary) fall through to
/// [`Self::Unknown`], which preserves the raw `name` and `value`
/// borrow so downstream tooling can still inspect them.
///
/// Adding a new annotation to the language: add a variant here, add
/// a match arm in [`Annotation::kind`], add a unit test for the
/// shape and the malformed fall-through cases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnnotationKind<'a> {
    /// `@doc "text"` or `@doc false`. Bare `@doc` is excluded — it
    /// has no consumer in the codebase and lands in
    /// [`Self::Unknown`].
    Doc(DocAttr),
    /// `@extern "C"` (today's only valid ABI). Future ABIs would
    /// surface here under different `abi` strings; the typecheck
    /// layer is responsible for restricting which ABIs are
    /// admissible.
    Extern { abi: &'a str },
    /// `@intrinsic` — compiler-emitted body, no source body, no FFI
    /// symbol. Carries no payload.
    Intrinsic,
    /// `@link "lib"` or `@link "lib:sym"`. `lib` is the bare library
    /// name (`-l<lib>` at link time); `name` is an optional C symbol
    /// override taken from the `lib:sym` shape.
    Link {
        lib: Option<&'a str>,
        name: Option<&'a str>,
    },
    /// `@test` — driver test-runner marker.
    Test,
    /// Anything else — unrecognized name, malformed value shape, or
    /// any annotation the compiler hasn't been taught about. Carries
    /// the raw `name` + `value` borrow so unrecognized-annotation
    /// diagnostics (a future slice) can render the original source.
    Unknown {
        name: &'a str,
        value: Option<&'a AnnotationValue>,
    },
}

impl Annotation {
    /// Classify this annotation against the compiler's known
    /// vocabulary. Pure function of `self.name` and `self.value`;
    /// runs in O(1) over a small fixed match. See
    /// [`AnnotationKind`] for the variant set and the "malformed
    /// shapes fall through to `Unknown`" contract.
    pub fn kind(&self) -> AnnotationKind<'_> {
        match self.name.as_str() {
            "doc" => match &self.value {
                Some(AnnotationValue::String(text)) => {
                    AnnotationKind::Doc(DocAttr::Text(text.clone()))
                }
                Some(AnnotationValue::False) => AnnotationKind::Doc(DocAttr::Suppressed),
                None => AnnotationKind::Unknown {
                    name: &self.name,
                    value: self.value.as_ref(),
                },
            },
            "extern" => match &self.value {
                Some(AnnotationValue::String(abi)) => AnnotationKind::Extern { abi },
                _ => AnnotationKind::Unknown {
                    name: &self.name,
                    value: self.value.as_ref(),
                },
            },
            "intrinsic" if self.value.is_none() => AnnotationKind::Intrinsic,
            "link" => match &self.value {
                Some(AnnotationValue::String(payload)) => match payload.split_once(':') {
                    Some((lib, name)) => AnnotationKind::Link {
                        lib: Some(lib),
                        name: Some(name),
                    },
                    None => AnnotationKind::Link {
                        lib: Some(payload.as_str()),
                        name: None,
                    },
                },
                _ => AnnotationKind::Unknown {
                    name: &self.name,
                    value: self.value.as_ref(),
                },
            },
            "test" if self.value.is_none() => AnnotationKind::Test,
            _ => AnnotationKind::Unknown {
                name: &self.name,
                value: self.value.as_ref(),
            },
        }
    }
}

/// Returns `true` when `annotations` contains an `@extern "C"` marker
/// (FFI-linked function with no source body). Thin wrapper over
/// [`Annotation::kind`]; kept as a free function because v1
/// (`expo-typecheck`, `expo-codegen`) binds against this signature
/// directly.
pub fn is_extern_c(annotations: &[Annotation]) -> bool {
    annotations
        .iter()
        .any(|a| matches!(a.kind(), AnnotationKind::Extern { abi: "C" }))
}

/// Returns `true` when `annotations` contains an `@intrinsic` marker
/// (compiler-emitted body, no source body, no FFI symbol). Thin
/// wrapper over [`Annotation::kind`]; kept as a free function
/// because v1 (`expo-typecheck`, `expo-codegen`) binds against this
/// signature directly.
pub fn is_intrinsic(annotations: &[Annotation]) -> bool {
    annotations
        .iter()
        .any(|a| matches!(a.kind(), AnnotationKind::Intrinsic))
}

/// A source comment preserved for formatting and documentation tooling.
#[derive(Debug, Clone)]
pub struct Comment {
    pub text: String,
    pub span: Span,
}

/// A compiler diagnostic emitted during parsing, type checking, or codegen.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub hint: Option<String>,
    pub span: Span,
}

impl Diagnostic {
    /// Build an `Error`-severity diagnostic with no hint.
    pub fn error(message: impl Into<String>, span: Span) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            hint: None,
            span,
        }
    }

    /// Build an `Error`-severity diagnostic carrying a hint.
    pub fn error_with_hint(
        message: impl Into<String>,
        hint: impl Into<String>,
        span: Span,
    ) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            hint: Some(hint.into()),
            span,
        }
    }

    /// Build a `Warning`-severity diagnostic with no hint.
    pub fn warning(message: impl Into<String>, span: Span) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
            hint: None,
            span,
        }
    }
}

/// A top-level declaration within a file.
// `Constant` dominates the discriminant size because it embeds an `Expr`
// for its RHS. Boxing it would ripple through every crate that matches
// `Item::Constant(_)` without a corresponding simplicity win -- these
// are transient AST nodes, not hot-path runtime values.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum Item {
    Alias(AliasDecl),
    Constant(Constant),
    Enum(EnumDecl),
    Function(Function),
    Impl(ImplBlock),
    Protocol(ProtocolDecl),
    Struct(StructDecl),
    TypeAlias(TypeAlias),
}

/// The root AST node representing a single Expo source file.
///
/// `package` is the post-parse identity that flows downstream through
/// typecheck and codegen; it's set by [`expo_parser::parse_file`] from
/// the originating `SourceFile.package`. Callers that go through the
/// bare-string [`expo_parser::parse`] entry point (REPL, formatter,
/// proptests) leave it `String::new()` -- those paths never reach the
/// package-scoped passes that read it.
///
/// `body` is `Some(_)` only when the source was parsed in
/// `ParseMode::Script` -- it carries top-level statements (e.g. the
/// REPL's accumulated input). The alpha pipeline keeps `body`
/// populated through typecheck and seal; downstream lowering
/// (`expo-alpha-ir::lower_script`) consumes it directly. Project-mode
/// (`ParseMode::File`) sources leave `body` as `None` and put their
/// work in `items`.
#[derive(Debug, Clone)]
pub struct File {
    pub body: Option<Vec<Statement>>,
    pub comments: Vec<Comment>,
    pub items: Vec<Item>,
    pub package: String,
    pub path: Option<PathBuf>,
    pub span: Span,
}

/// The severity level of a compiler diagnostic.
#[derive(Debug, Clone, PartialEq)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

// Declarations

/// A generic type parameter with optional protocol bounds.
///
/// ```expo
/// fn format<T: Debug>(item: T)          # bounded
/// fn identity<T>(item: T) -> T          # unbounded
/// fn dedup<T: Equality & Hash>(items: List<T>)  # multiple bounds
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct TypeParam {
    pub name: String,
    pub bounds: Vec<String>,
    pub span: Span,
}

/// A package-level constant: `const NAME = expr` or `const NAME: Type = expr`.
#[derive(Debug, Clone)]
pub struct Constant {
    pub annotations: Vec<Annotation>,
    pub name: String,
    pub type_annotation: Option<TypeExpr>,
    pub value: Expr,
    pub span: Span,
}

/// An enum declaration: `enum Color ... end`.
#[derive(Debug, Clone)]
pub struct EnumDecl {
    pub annotations: Vec<Annotation>,
    pub name: String,
    pub type_params: Vec<TypeParam>,
    pub variants: Vec<EnumVariant>,
    pub functions: Vec<Function>,
    pub span: Span,
}

/// A single variant within an enum declaration.
#[derive(Debug, Clone)]
pub struct EnumVariant {
    pub name: String,
    pub data: EnumVariantData,
    pub span: Span,
}

/// The data shape of an enum variant.
#[derive(Debug, Clone)]
pub enum EnumVariantData {
    /// A unit variant carrying no data: `None`.
    Unit,
    /// A tuple variant: `Some(Int)`.
    Tuple(Vec<TypeExpr>),
    /// A struct variant with named fields: `Move { x: Int, y: Int }`.
    Struct(Vec<StructField>),
}

/// A function declaration: `fn name(params) -> ReturnType ... end`.
/// `body` is `None` for extern declarations (`@extern "C"`) that have no body.
#[derive(Debug, Clone)]
pub struct Function {
    pub annotations: Vec<Annotation>,
    pub visibility: Visibility,
    pub name: String,
    pub type_params: Vec<TypeParam>,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub body: Option<Vec<Statement>>,
    pub span: Span,
}

/// An `impl` block attaching methods to a struct or enum.
#[derive(Debug, Clone)]
pub struct ImplBlock {
    pub target: TypeExpr,
    pub trait_expr: Option<TypeExpr>,
    pub members: Vec<ImplMember>,
    pub span: Span,
}

/// A member within an `impl` block.
#[derive(Debug, Clone)]
pub enum ImplMember {
    Function(Function),
    TypeAlias(TypeAlias),
}

/// A protocol declaration: `protocol Display ... end`.
#[derive(Debug, Clone)]
pub struct ProtocolDecl {
    pub annotations: Vec<Annotation>,
    pub name: String,
    pub type_params: Vec<TypeParam>,
    pub methods: Vec<ProtocolMethod>,
    pub span: Span,
}

/// A method within a protocol declaration.
/// If `body` is `None`, the method is required (implementors must provide it).
/// If `body` is `Some`, it serves as the default implementation.
#[derive(Debug, Clone)]
pub struct ProtocolMethod {
    pub annotations: Vec<Annotation>,
    pub name: String,
    pub type_params: Vec<TypeParam>,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub body: Option<Vec<Statement>>,
    pub span: Span,
}

/// A function parameter: either a `self` receiver or a named parameter.
///
/// Both variants carry a `local_id: Option<LocalId>` slot the parser
/// leaves as `None`; alpha typecheck's `resolve_function` stamps it in
/// when the param enters the per-function `LocalScope`. IR lower reads
/// the stamped id (translating to `IRLocalId`) so body references and
/// param-promotion `LocalDecl`/`LocalWrite`s share the same handle
/// without crate-boundary leakage.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Param {
    /// A regular named parameter with an optional default value.
    /// `move name: Type` uses [`PassMode::Move`]; plain `name: Type` uses
    /// [`PassMode::Borrow`].
    Regular {
        mode: PassMode,
        name: String,
        type_expr: TypeExpr,
        default: Option<Expr>,
        local_id: Option<LocalId>,
        span: Span,
    },
    /// The `self` receiver: `self` ([`PassMode::Borrow`]) or `move self`
    /// ([`PassMode::Move`]).
    Self_ {
        mode: PassMode,
        local_id: Option<LocalId>,
        span: Span,
    },
}

/// A struct declaration: `struct Point ... end`.
#[derive(Debug, Clone)]
pub struct StructDecl {
    pub annotations: Vec<Annotation>,
    pub name: String,
    pub type_params: Vec<TypeParam>,
    pub fields: Vec<StructField>,
    pub functions: Vec<Function>,
    pub span: Span,
}

/// A single field within a struct declaration.
#[derive(Debug, Clone)]
pub struct StructField {
    pub name: String,
    pub type_expr: TypeExpr,
    pub default: Option<Expr>,
    pub span: Span,
}

/// A file-private alias for a package-qualified type: `alias json.Decoder`
/// or `alias json.Decoder as JSONDecoder`.
#[derive(Debug, Clone)]
pub struct AliasDecl {
    pub path: Vec<String>,
    pub local_name: String,
    pub span: Span,
}

/// A type alias within an `impl` block: `type Name = TypeExpr`.
#[derive(Debug, Clone)]
pub struct TypeAlias {
    pub annotations: Vec<Annotation>,
    pub name: String,
    pub type_expr: TypeExpr,
    pub span: Span,
}

// Type expressions

/// A type annotation in source code (e.g., `Int`, `List<String>`).
#[derive(Debug, Clone)]
pub enum TypeExpr {
    /// A simple named type: `Int`, `String`, `MyStruct`.
    Named { path: Vec<String>, span: Span },
    /// A generic type with type arguments: `List<Int>`, `Map<String, Int>`.
    Generic {
        path: Vec<String>,
        args: Vec<TypeExpr>,
        span: Span,
    },
    /// The unit type: `()`.
    Unit { span: Span },
    /// A function type: `fn (Int32, String) -> Bool`.
    /// `param_modes` tracks the [`PassMode`] per parameter position
    /// (e.g. `fn (move T) -> U` produces `[PassMode::Move]`).
    Function {
        params: Vec<TypeExpr>,
        param_modes: Vec<PassMode>,
        return_type: Box<TypeExpr>,
        span: Span,
    },
    /// The `Self` type: resolves to the implementing type inside `impl` and
    /// `protocol` blocks.
    Self_ { span: Span },
    /// A union type: `A | B | C`.
    Union { types: Vec<TypeExpr>, span: Span },
}

// Statements

/// The left-hand side of an assignment.
#[derive(Debug, Clone)]
pub enum AssignTarget {
    /// A simple or dotted lvalue: `x`, `point.x`.
    LValue(LValue),
    /// A destructuring pattern: `[a, b] = expr`.
    Pattern(Pattern),
}

/// Compound assignment operators: `+=`, `-=`, `*=`, `/=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompoundOp {
    Add,
    Div,
    Mul,
    Sub,
}

/// A dotted lvalue path used in assignments: `x`, `point.x`, `self.name`.
///
/// `local_id` is `None` after parse and stamped by typecheck-resolve
/// when the segments resolve to a single-segment local binding (`x`,
/// not `point.x`). The IR lower path keys its `LocalDecl` /
/// `LocalWrite` instructions on the [`LocalId`] and never reaches
/// back into [`Self::segments`] — keeps the seam clean and lets a
/// future block-scoping slice rename via the id without the surface
/// `String` going stale.
#[derive(Debug, Clone)]
pub struct LValue {
    pub local_id: Option<LocalId>,
    pub segments: Vec<String>,
    pub span: Span,
}

/// A statement within a function or block body.
#[derive(Debug, Clone)]
pub enum Statement {
    /// A bare expression evaluated for its side effects.
    Expr(Expr),
    /// A variable or pattern assignment: `x = expr`, `x: Type = expr`.
    Assignment {
        target: AssignTarget,
        type_annotation: Option<TypeExpr>,
        value: Expr,
        span: Span,
    },
    /// A compound assignment: `x += 1`.
    CompoundAssign {
        target: LValue,
        op: CompoundOp,
        value: Expr,
        span: Span,
    },
    /// An explicit return: `return expr`.
    Return { value: Option<Expr>, span: Span },
    /// A loop break: `break`.
    Break { span: Span },
}

// Expressions

/// A positional or named argument in a function/method call.
#[derive(Debug, Clone)]
pub struct Arg {
    pub name: Option<String>,
    pub value: Expr,
    pub span: Span,
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    And,
    Concat,
    Div,
    Eq,
    Gt,
    GtEq,
    Lt,
    LtEq,
    Mod,
    Mul,
    NotEq,
    Or,
    Sub,
}

/// A parameter in a closure expression.
#[derive(Debug, Clone)]
pub enum ClosureParam {
    /// A named parameter with optional type: `x`, `x: Int`, `move x: Int`.
    Name {
        mode: PassMode,
        name: String,
        type_expr: Option<TypeExpr>,
        span: Span,
    },
    /// A destructuring parameter: `(a, b)`.
    Destructured { names: Vec<String>, span: Span },
    /// A wildcard parameter: `_`.
    Wildcard { span: Span },
}

/// The data payload when constructing an enum variant value.
#[derive(Debug, Clone)]
pub enum EnumConstructionData {
    /// No data: `Color.Red`.
    Unit,
    /// Positional data: `Option.Some(42)`.
    Tuple(Vec<Expr>),
    /// Named fields: `Shape.Rect { width: 10, height: 20 }`.
    Struct(Vec<FieldInit>),
}

/// An expression node in the AST.
///
/// Every expression carries a `span` for source location and **two**
/// type-annotation slots that together describe the expression's type
/// as known by the compiler:
///
/// - `resolved_type`: **legacy (v1 pipeline).** Populated by
///   `expo-typecheck`; read by v1 codegen, LSP, and IR. Uses the
///   closed `Type` enum from `expo_ast::types`. Alpha does **not**
///   populate this field.
/// - `resolution`: **northstar (alpha pipeline).** Populated by
///   `expo-alpha-typecheck`; carries a [`ResolvedType`] that points
///   into the alpha `GlobalRegistry` by id. Every sealed alpha `Expr`
///   has `resolution.is_resolved()` true. V1 does **not** populate
///   this field.
///
/// The two fields coexist during the v1 -> alpha migration so both
/// pipelines can operate on the same AST shape without clobbering
/// each other's state. Once v1 is retired, `resolved_type` goes away
/// and `resolution` is the single ledger.
#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
    /// Legacy v1 type annotation. `None` before v1 typecheck runs.
    /// Alpha does not populate this field.
    pub resolved_type: Option<Type>,
    /// Northstar-aligned type annotation. Default is
    /// [`ResolvedType::unresolved`]; alpha resolve populates it with
    /// a registry-pointing shape, and seal asserts
    /// `resolution.is_resolved()` on every non-excluded node.
    pub resolution: ResolvedType,
}

impl Expr {
    /// Convenience constructor: wraps a kind + span with both type
    /// annotations defaulted (legacy `resolved_type: None`,
    /// northstar `resolution: Unresolved`).
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Self {
            kind,
            span,
            resolved_type: None,
            resolution: ResolvedType::unresolved(),
        }
    }
}

/// The specific kind of an expression node.
#[derive(Debug, Clone)]
pub enum ExprKind {
    /// A binary operation: `a + b`, `x * y`.
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// A binary/bitstring literal: `<<0xFF, 0x00, length::16>>`.
    BinaryLiteral { segments: Vec<BinarySegment> },
    /// A function call: `f(args)`.
    ///
    /// `type_args` is empty after parse and stamped by typecheck:
    /// for a generic callee `fn id<T>(x: T)`, the inferred concrete
    /// type for each declared param lands here in declaration order
    /// so IR lower can spawn the right monomorphization. Non-generic
    /// callees keep it empty.
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
        type_args: Vec<ResolvedType>,
    },
    /// A block closure: `fn (x: Int) -> Int ... end`.
    Closure {
        params: Vec<ClosureParam>,
        return_type: Option<TypeExpr>,
        body: Vec<Statement>,
    },
    /// A multi-branch conditional: `cond ... end`.
    Cond {
        arms: Vec<CondArm>,
        else_body: Option<Vec<Statement>>,
    },
    /// An enum variant construction: `Color.Red`, `Option.Some(42)`.
    EnumConstruction {
        type_path: Vec<String>,
        variant: String,
        data: EnumConstructionData,
    },
    /// A field access: `point.x`.
    FieldAccess { receiver: Box<Expr>, field: String },
    /// A for loop: `for x in items ... end`.
    For {
        pattern: Pattern,
        iterable: Box<Expr>,
        body: Vec<Statement>,
    },
    /// A parenthesized grouping: `(expr)`.
    Group { expr: Box<Expr> },
    /// A variable reference: `x`, `my_var`.
    Ident {
        name: String,
        resolution: Resolution,
    },
    /// An if/else expression: `if cond ... end`.
    If {
        condition: Box<Expr>,
        then_body: Vec<Statement>,
        else_body: Option<Vec<Statement>>,
    },
    /// A list literal: `[1, 2, 3]`.
    List { elements: Vec<Expr> },
    /// A map literal: `["key": value, ...]` or `[:]` for an empty map.
    Map { entries: Vec<(Expr, Expr)> },
    /// A literal value: `42`, `true`, `"hello"`.
    Literal { value: Literal },
    /// An infinite loop: `loop ... end`.
    Loop { body: Vec<Statement> },
    /// A pattern match expression: `match subject ... end`.
    Match {
        subject: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    /// A method or qualified call: `obj.method(args)`, `math.add(1, 2)`.
    /// `type_args` follows the same shape as [`ExprKind::Call`].
    MethodCall {
        receiver: Box<Expr>,
        method: String,
        args: Vec<Arg>,
        type_args: Vec<ResolvedType>,
    },
    /// A receive block with match arms and optional timeout:
    /// `receive ... after timeout -> ... end`.
    Receive {
        arms: Vec<MatchArm>,
        after_timeout: Option<Box<Expr>>,
        after_body: Vec<Statement>,
    },
    /// A self reference: `self`. `local_id` is `None` after parse and
    /// stamped by typecheck-resolve to the enclosing instance method's
    /// `self` slot. IR lower keys its `LocalRead` on the same id, so
    /// `self.field` and `self` references thread through the same
    /// local-slot vocabulary as body-declared locals.
    Self_ { local_id: Option<LocalId> },
    /// An inline closure: `x -> x * 2`.
    ShortClosure {
        params: Vec<ClosureParam>,
        body: Box<Expr>,
    },
    /// A spawn expression: `spawn expr`.
    Spawn { expr: Box<Expr> },
    /// A string literal, possibly with interpolation: `"hello #{name}"`.
    String {
        parts: Vec<StringPart>,
        multiline: bool,
    },
    /// A struct construction: `Point { x: 1, y: 2 }`.
    StructConstruction {
        type_path: Vec<String>,
        fields: Vec<FieldInit>,
    },
    /// A ternary expression: `cond ? then_expr : else_expr`.
    Ternary {
        condition: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
    },
    /// A unary operation: `-x`, `not flag`.
    Unary { op: UnaryOp, operand: Box<Expr> },
    /// An unless guard: `unless cond ... end`.
    Unless {
        condition: Box<Expr>,
        body: Vec<Statement>,
    },
    /// A while loop: `while cond ... end`.
    While {
        condition: Box<Expr>,
        body: Vec<Statement>,
    },
}

/// A named field initializer in a struct or enum struct construction.
#[derive(Debug, Clone)]
pub struct FieldInit {
    pub name: String,
    pub value: Expr,
    pub span: Span,
}

/// A literal value in source code.
#[derive(Debug, Clone)]
pub enum Literal {
    Bool(bool),
    Float(String),
    Int(String),
    String(String),
    Unit,
}

/// A segment of a string literal, either raw text or an interpolation.
#[derive(Debug, Clone)]
pub enum StringPart {
    /// A raw text fragment within a string.
    Literal { value: String, span: Span },
    /// An interpolated expression: `#{expr}`.
    Interpolation {
        expr: Box<Expr>,
        format: Option<String>,
        span: Span,
    },
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

// Binary literals

/// Whether a binary segment size is measured in bits (default) or bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryUnit {
    Bit,
    Byte,
}

/// Signedness modifier for a binary segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinarySignedness {
    Signed,
    Unsigned,
}

/// Endianness modifier for a binary segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryEndianness {
    Big,
    Little,
}

/// A single segment within a `<<...>>` binary literal or pattern.
#[derive(Debug, Clone)]
pub struct BinarySegment {
    pub value: Box<Expr>,
    pub size: Option<Box<Expr>>,
    pub unit: BinaryUnit,
    pub signedness: Option<BinarySignedness>,
    pub endianness: Option<BinaryEndianness>,
    pub type_ann: Option<TypeExpr>,
    pub span: Span,
}

// Arms

/// A single branch in a `cond` expression.
#[derive(Debug, Clone)]
pub struct CondArm {
    pub condition: Expr,
    pub body: Vec<Statement>,
    pub span: Span,
}

/// A single branch in a `match` expression with a pattern and optional guard.
#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Vec<Statement>,
    pub span: Span,
}

// Patterns

/// A named field within a struct pattern (either `Pattern::Struct` or
/// `Pattern::EnumStruct`). Form is always `name: pattern` -- there is no
/// shorthand. To bind under the field name, write `name: name`; to ignore,
/// write `name: _` or omit the field entirely (partial coverage).
#[derive(Debug, Clone)]
pub struct FieldPattern {
    pub name: String,
    pub pattern: Pattern,
    pub span: Span,
}

/// A destructuring pattern used in `match` arms, `for` loops, and assignments.
#[derive(Debug, Clone)]
pub enum Pattern {
    /// A wildcard that matches anything: `_`.
    Wildcard { span: Span },
    /// A literal match: `42`, `true`.
    Literal { value: Literal, span: Span },
    /// A binary/bitstring pattern: `<<header::8, payload::16 big>>`.
    Binary {
        segments: Vec<BinarySegment>,
        span: Span,
    },
    /// A variable binding: `x`, `name`. `local_id` is `None` after
    /// parse and stamped by typecheck resolve, mirroring the
    /// [`Param::Regular`] / [`ExprKind::Self_`] slot.
    Binding {
        local_id: Option<LocalId>,
        name: String,
        span: Span,
    },
    /// A unit enum variant: `Color.Red`.
    EnumUnit {
        type_path: Vec<String>,
        variant: String,
        span: Span,
        /// Resolved identity of the enum type. Populated by the type checker.
        resolved_type: Option<TypeIdentifier>,
    },
    /// A tuple enum variant: `Option.Some(x)`.
    EnumTuple {
        type_path: Vec<String>,
        variant: String,
        elements: Vec<Pattern>,
        span: Span,
        /// Resolved identity of the enum type. Populated by the type checker.
        resolved_type: Option<TypeIdentifier>,
    },
    /// A struct enum variant: `Shape.Rect { width, height }`.
    EnumStruct {
        type_path: Vec<String>,
        variant: String,
        fields: Vec<FieldPattern>,
        span: Span,
        /// Resolved identity of the enum type. Populated by the type checker.
        resolved_type: Option<TypeIdentifier>,
    },
    /// Shorthand constructors: `Some(x)`, `Ok(x)`, `Err(x)`.
    Constructor {
        name: String,
        elements: Vec<Pattern>,
        span: Span,
        /// Resolved identity of the enum type. Populated by the type checker.
        resolved_type: Option<TypeIdentifier>,
    },
    /// A plain (non-enum) struct destructuring: `Point{x: 5, y: 2}`.
    /// Field syntax is always `name: pattern` (no shorthand binding).
    /// Unlisted fields are implicit wildcards. Empty `Point{}` is legal
    /// and matches any value of that struct type.
    Struct {
        type_path: Vec<String>,
        fields: Vec<FieldPattern>,
        span: Span,
        /// Resolved identity of the struct type. Populated by the type checker.
        resolved_type: Option<TypeIdentifier>,
    },
    /// A typed binding: `p: Post` -- matches a union member by type
    /// and binds the unwrapped value.
    TypedBinding {
        name: String,
        type_expr: TypeExpr,
        span: Span,
    },
    /// A list pattern: `[head, tail]`.
    List { elements: Vec<Pattern>, span: Span },
    /// An OR pattern: `1 | 2 | 3`.
    Or { patterns: Vec<Pattern>, span: Span },
}

#[cfg(test)]
mod annotation_tests {
    use super::*;

    fn ann(name: &str, value: Option<AnnotationValue>) -> Annotation {
        Annotation {
            name: name.to_string(),
            value,
            span: Span::default(),
        }
    }

    fn str_value(s: &str) -> Option<AnnotationValue> {
        Some(AnnotationValue::String(s.to_string()))
    }

    #[test]
    fn extern_c_classifies_as_extern_with_abi() {
        let a = ann("extern", str_value("C"));
        assert_eq!(a.kind(), AnnotationKind::Extern { abi: "C" });
    }

    #[test]
    fn extern_with_other_abi_carries_through_payload() {
        let a = ann("extern", str_value("rust"));
        assert_eq!(a.kind(), AnnotationKind::Extern { abi: "rust" });
    }

    #[test]
    fn extern_with_false_value_falls_through_to_unknown() {
        let a = ann("extern", Some(AnnotationValue::False));
        assert!(matches!(
            a.kind(),
            AnnotationKind::Unknown {
                name: "extern",
                value: Some(AnnotationValue::False),
            }
        ));
    }

    #[test]
    fn intrinsic_bare_classifies() {
        let a = ann("intrinsic", None);
        assert_eq!(a.kind(), AnnotationKind::Intrinsic);
    }

    #[test]
    fn intrinsic_with_value_falls_through_to_unknown() {
        let a = ann("intrinsic", str_value("foo"));
        assert!(matches!(
            a.kind(),
            AnnotationKind::Unknown {
                name: "intrinsic",
                ..
            }
        ));
    }

    #[test]
    fn link_lib_only_parses_into_lib_field() {
        let a = ann("link", str_value("m"));
        assert_eq!(
            a.kind(),
            AnnotationKind::Link {
                lib: Some("m"),
                name: None,
            },
        );
    }

    #[test]
    fn link_lib_with_symbol_splits_on_first_colon() {
        let a = ann("link", str_value("m:cosf"));
        assert_eq!(
            a.kind(),
            AnnotationKind::Link {
                lib: Some("m"),
                name: Some("cosf"),
            },
        );
    }

    #[test]
    fn link_with_false_value_falls_through_to_unknown() {
        let a = ann("link", Some(AnnotationValue::False));
        assert!(matches!(
            a.kind(),
            AnnotationKind::Unknown { name: "link", .. }
        ));
    }

    #[test]
    fn doc_string_classifies_as_text() {
        let a = ann("doc", str_value("hello"));
        assert_eq!(
            a.kind(),
            AnnotationKind::Doc(DocAttr::Text("hello".to_string())),
        );
    }

    #[test]
    fn doc_false_classifies_as_suppressed() {
        let a = ann("doc", Some(AnnotationValue::False));
        assert_eq!(a.kind(), AnnotationKind::Doc(DocAttr::Suppressed));
    }

    #[test]
    fn bare_doc_falls_through_to_unknown() {
        // Regression test for the deliberate behavior shift away from
        // the legacy `is_doc_annotation`: bare `@doc` is now treated
        // as a malformed annotation rather than a meaningless
        // documentation marker.
        let a = ann("doc", None);
        assert!(matches!(
            a.kind(),
            AnnotationKind::Unknown {
                name: "doc",
                value: None
            }
        ));
    }

    #[test]
    fn test_marker_classifies() {
        let a = ann("test", None);
        assert_eq!(a.kind(), AnnotationKind::Test);
    }

    #[test]
    fn test_with_value_falls_through_to_unknown() {
        let a = ann("test", str_value("x"));
        assert!(matches!(
            a.kind(),
            AnnotationKind::Unknown { name: "test", .. }
        ));
    }

    #[test]
    fn unrecognized_name_classifies_as_unknown() {
        let a = ann("custom", str_value("payload"));
        assert!(matches!(
            a.kind(),
            AnnotationKind::Unknown { name: "custom", .. }
        ));
    }

    #[test]
    fn is_extern_c_wrapper_matches_kind() {
        assert!(is_extern_c(&[ann("extern", str_value("C"))]));
        assert!(!is_extern_c(&[ann("extern", str_value("rust"))]));
        assert!(!is_extern_c(&[ann("intrinsic", None)]));
        assert!(!is_extern_c(&[]));
    }

    #[test]
    fn is_intrinsic_wrapper_matches_kind() {
        assert!(is_intrinsic(&[ann("intrinsic", None)]));
        assert!(!is_intrinsic(&[ann("intrinsic", str_value("foo"))]));
        assert!(!is_intrinsic(&[ann("extern", str_value("C"))]));
        assert!(!is_intrinsic(&[]));
    }
}
