//! The Expo abstract syntax tree.
//!
//! A single source file parses into a [`Module`], which contains a list of
//! top-level [`Item`]s (functions, structs, enums, imports, constants, impls).
//! Functions hold a body of [`Statement`]s, which in turn contain [`Expr`]
//! nodes. [`Pattern`]s appear in `match` arms, `for` loops, and destructuring
//! assignments.

use std::path::PathBuf;

use crate::span::Span;

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

/// Access control for functions: public by default, `priv` for module-private.
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
    /// Accessible from other modules.
    Public,
    /// Accessible only within the declaring module.
    Private,
}

// Top level

/// The value attached to an annotation.
#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationValue {
    /// A string value: `@doc "text"` or `@doc """text"""`.
    String(String),
    /// An explicit false: `@doc false` — suppresses documentation.
    False,
}

/// A metadata annotation such as `@doc` or `@moduledoc`.
#[derive(Debug, Clone)]
pub struct Annotation {
    pub name: String,
    pub value: Option<AnnotationValue>,
    pub span: Span,
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

/// A top-level declaration within a module.
#[derive(Debug, Clone)]
pub enum Item {
    Alias(AliasDecl),
    Constant(Constant),
    Enum(EnumDecl),
    Function(Function),
    Impl(ImplBlock),
    Protocol(ProtocolDecl),
    Shared(SharedDecl),
    Struct(StructDecl),
    TypeAlias(TypeAlias),
}

/// The root AST node representing a single Expo source file.
#[derive(Debug, Clone)]
pub struct Module {
    pub items: Vec<Item>,
    pub comments: Vec<Comment>,
    pub span: Span,
    pub path: Option<PathBuf>,
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

/// A module-level constant: `const NAME = expr` or `const NAME: Type = expr`.
#[derive(Debug, Clone)]
pub struct Constant {
    pub annotation: Option<Annotation>,
    pub name: String,
    pub type_annotation: Option<TypeExpr>,
    pub value: Expr,
    pub span: Span,
}

/// An enum declaration: `enum Color ... end`.
#[derive(Debug, Clone)]
pub struct EnumDecl {
    pub annotation: Option<Annotation>,
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
#[derive(Debug, Clone)]
pub struct Function {
    pub annotation: Option<Annotation>,
    pub visibility: Visibility,
    pub name: String,
    pub type_params: Vec<TypeParam>,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub body: Vec<Statement>,
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
    pub annotation: Option<Annotation>,
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
    pub annotation: Option<Annotation>,
    pub name: String,
    pub type_params: Vec<TypeParam>,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub body: Option<Vec<Statement>>,
    pub span: Span,
}

/// A function parameter: either a `self` receiver or a named parameter.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Param {
    /// The `self` receiver: `self` ([`PassMode::Borrow`]) or `move self`
    /// ([`PassMode::Move`]).
    Self_ { mode: PassMode, span: Span },
    /// A regular named parameter with an optional default value.
    /// `move name: Type` uses [`PassMode::Move`]; plain `name: Type` uses
    /// [`PassMode::Borrow`].
    Regular {
        mode: PassMode,
        name: String,
        type_expr: TypeExpr,
        default: Option<Expr>,
        span: Span,
    },
}

/// A `shared` declaration for concurrent shared state.
#[derive(Debug, Clone)]
pub struct SharedDecl {
    pub name: String,
    pub type_expr: TypeExpr,
    pub span: Span,
}

/// A struct declaration: `struct Point ... end`.
#[derive(Debug, Clone)]
pub struct StructDecl {
    pub annotation: Option<Annotation>,
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
    pub annotation: Option<Annotation>,
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
#[derive(Debug, Clone)]
pub struct LValue {
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
#[derive(Debug, Clone)]
pub enum Expr {
    /// An arena allocation block: `arena ... end`.
    Arena { body: Vec<Statement>, span: Span },
    /// A binary operation: `a + b`, `x * y`.
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
        span: Span,
    },
    /// A binary/bitstring literal: `<<0xFF, 0x00, length::16>>`.
    BinaryLiteral {
        segments: Vec<BinarySegment>,
        span: Span,
    },
    /// A function call: `f(args)`.
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
        span: Span,
    },
    /// A block closure: `fn (x: Int) -> Int ... end`.
    Closure {
        params: Vec<ClosureParam>,
        return_type: Option<TypeExpr>,
        body: Vec<Statement>,
        span: Span,
    },
    /// A multi-branch conditional: `cond ... end`.
    Cond {
        arms: Vec<CondArm>,
        else_body: Option<Vec<Statement>>,
        span: Span,
    },
    /// An enum variant construction: `Color.Red`, `Option.Some(42)`.
    EnumConstruction {
        type_path: Vec<String>,
        variant: String,
        data: EnumConstructionData,
        span: Span,
    },
    /// A field access: `point.x`.
    FieldAccess {
        receiver: Box<Expr>,
        field: String,
        span: Span,
    },
    /// A for loop: `for x in items ... end`.
    For {
        pattern: Pattern,
        iterable: Box<Expr>,
        body: Vec<Statement>,
        span: Span,
    },
    /// A parenthesized grouping: `(expr)`.
    Group { expr: Box<Expr>, span: Span },
    /// A variable reference: `x`, `my_var`.
    Ident { name: String, span: Span },
    /// An if/else expression: `if cond ... end`.
    If {
        condition: Box<Expr>,
        then_body: Vec<Statement>,
        else_body: Option<Vec<Statement>>,
        span: Span,
    },
    /// A list literal: `[1, 2, 3]`.
    List { elements: Vec<Expr>, span: Span },
    /// A map literal: `["key": value, ...]` or `[:]` for an empty map.
    Map {
        entries: Vec<(Expr, Expr)>,
        span: Span,
    },
    /// A literal value: `42`, `true`, `"hello"`.
    Literal { value: Literal, span: Span },
    /// An infinite loop: `loop ... end`.
    Loop { body: Vec<Statement>, span: Span },
    /// A pattern match expression: `match subject ... end`.
    Match {
        subject: Box<Expr>,
        arms: Vec<MatchArm>,
        span: Span,
    },
    /// A method or qualified call: `obj.method(args)`, `math.add(1, 2)`.
    MethodCall {
        receiver: Box<Expr>,
        method: String,
        args: Vec<Arg>,
        span: Span,
    },
    /// A receive block with match arms and optional timeout:
    /// `receive ... after timeout -> ... end`.
    Receive {
        arms: Vec<MatchArm>,
        after_timeout: Option<Box<Expr>>,
        after_body: Vec<Statement>,
        span: Span,
    },
    /// A self reference: `self`.
    Self_ { span: Span },
    /// An inline closure: `x -> x * 2`.
    ShortClosure {
        params: Vec<ClosureParam>,
        body: Box<Expr>,
        span: Span,
    },
    /// A spawn expression: `spawn expr`.
    Spawn { expr: Box<Expr>, span: Span },
    /// A string literal, possibly with interpolation: `"hello #{name}"`.
    String {
        parts: Vec<StringPart>,
        multiline: bool,
        span: Span,
    },
    /// A struct construction: `Point { x: 1, y: 2 }`.
    StructConstruction {
        type_path: Vec<String>,
        fields: Vec<FieldInit>,
        span: Span,
    },
    /// A ternary expression: `cond ? then_expr : else_expr`.
    Ternary {
        condition: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
        span: Span,
    },
    /// A unary operation: `-x`, `not flag`.
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
        span: Span,
    },
    /// An unless guard: `unless cond ... end`.
    Unless {
        condition: Box<Expr>,
        body: Vec<Statement>,
        span: Span,
    },
    /// A while loop: `while cond ... end`.
    While {
        condition: Box<Expr>,
        body: Vec<Statement>,
        span: Span,
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
        expr: Expr,
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

/// A named field within an enum struct pattern: `name: pattern` or shorthand `name`.
#[derive(Debug, Clone)]
pub struct FieldPattern {
    pub name: String,
    pub pattern: Option<Pattern>,
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
    /// A variable binding: `x`, `name`.
    Binding { name: String, span: Span },
    /// A unit enum variant: `Color.Red`.
    EnumUnit {
        type_path: Vec<String>,
        variant: String,
        span: Span,
    },
    /// A tuple enum variant: `Option.Some(x)`.
    EnumTuple {
        type_path: Vec<String>,
        variant: String,
        elements: Vec<Pattern>,
        span: Span,
    },
    /// A struct enum variant: `Shape.Rect { width, height }`.
    EnumStruct {
        type_path: Vec<String>,
        variant: String,
        fields: Vec<FieldPattern>,
        span: Span,
    },
    /// Shorthand constructors: `Some(x)`, `Ok(x)`, `Err(x)`.
    Constructor {
        name: String,
        elements: Vec<Pattern>,
        span: Span,
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
