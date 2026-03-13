use crate::span::Span;

// ============================================================================
// Top level
// ============================================================================

#[derive(Debug, Clone)]
pub struct Module {
    pub items: Vec<Item>,
    pub comments: Vec<Comment>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Item {
    Constant(Constant),
    Enum(EnumDecl),
    Function(Function),
    Impl(ImplBlock),
    Import(Import),
    Shared(SharedDecl),
    Struct(StructDecl),
}

#[derive(Debug, Clone)]
pub struct Annotation {
    pub name: String,
    pub value: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Comment {
    pub text: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub hint: Option<String>,
    pub span: Span,
}

// ============================================================================
// Imports
// ============================================================================

#[derive(Debug, Clone)]
pub struct Import {
    pub path: Vec<String>,
    pub target: ImportTarget,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ImportTarget {
    Module,
    Item(String),
    Group(Vec<String>),
    Wildcard,
}

// ============================================================================
// Declarations
// ============================================================================

#[derive(Debug, Clone)]
pub struct StructDecl {
    pub annotation: Option<Annotation>,
    pub name: String,
    pub type_params: Vec<String>,
    pub fields: Vec<StructField>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct StructField {
    pub name: String,
    pub type_expr: TypeExpr,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct EnumDecl {
    pub annotation: Option<Annotation>,
    pub name: String,
    pub type_params: Vec<String>,
    pub variants: Vec<EnumVariant>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct EnumVariant {
    pub name: String,
    pub data: EnumVariantData,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum EnumVariantData {
    Unit,
    Tuple(Vec<TypeExpr>),
    Struct(Vec<StructField>),
}

#[derive(Debug, Clone)]
pub struct ImplBlock {
    pub target: TypeExpr,
    pub trait_expr: Option<TypeExpr>,
    pub members: Vec<ImplMember>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ImplMember {
    Function(Function),
    TypeAlias(TypeAlias),
}

#[derive(Debug, Clone)]
pub struct TypeAlias {
    pub name: String,
    pub type_expr: TypeExpr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub annotation: Option<Annotation>,
    pub is_private: bool,
    pub name: String,
    pub type_params: Vec<String>,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub body: Vec<Statement>,
    pub span: Span,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Param {
    Self_ {
        span: Span,
    },
    Regular {
        is_move: bool,
        name: String,
        type_expr: TypeExpr,
        default: Option<Expr>,
        span: Span,
    },
}

#[derive(Debug, Clone)]
pub struct Constant {
    pub name: String,
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct SharedDecl {
    pub name: String,
    pub type_expr: TypeExpr,
    pub span: Span,
}

// ============================================================================
// Type expressions
// ============================================================================

#[derive(Debug, Clone)]
pub enum TypeExpr {
    Named {
        path: Vec<String>,
        span: Span,
    },
    Generic {
        path: Vec<String>,
        args: Vec<TypeExpr>,
        span: Span,
    },
    Ref {
        inner: Box<TypeExpr>,
        span: Span,
    },
    Tuple {
        elements: Vec<TypeExpr>,
        span: Span,
    },
    Unit {
        span: Span,
    },
}

// ============================================================================
// Statements
// ============================================================================

#[derive(Debug, Clone)]
pub enum Statement {
    Expr(Expr),
    Assignment {
        target: AssignTarget,
        value: Expr,
        span: Span,
    },
    CompoundAssign {
        target: LValue,
        op: CompoundOp,
        value: Expr,
        span: Span,
    },
    Return {
        value: Option<Expr>,
        span: Span,
    },
    Break {
        span: Span,
    },
}

#[derive(Debug, Clone)]
pub enum AssignTarget {
    LValue(LValue),
    Pattern(Pattern),
}

#[derive(Debug, Clone)]
pub struct LValue {
    pub segments: Vec<String>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompoundOp {
    Add,
    Div,
    Mul,
    Sub,
}

// ============================================================================
// Expressions
// ============================================================================

#[derive(Debug, Clone)]
pub enum Expr {
    Arena {
        body: Vec<Statement>,
        span: Span,
    },
    Await {
        expr: Box<Expr>,
        span: Span,
    },
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
        span: Span,
    },
    Call {
        callee: Box<Expr>,
        type_args: Option<Vec<TypeExpr>>,
        args: Vec<Arg>,
        span: Span,
    },
    Closure {
        params: Vec<ClosureParam>,
        body: Vec<Statement>,
        span: Span,
    },
    Cond {
        arms: Vec<CondArm>,
        span: Span,
    },
    EnumConstruction {
        type_path: Vec<String>,
        variant: String,
        data: EnumConstructionData,
        span: Span,
    },
    FieldAccess {
        receiver: Box<Expr>,
        field: String,
        span: Span,
    },
    For {
        pattern: Pattern,
        iterable: Box<Expr>,
        body: Vec<Statement>,
        span: Span,
    },
    Group {
        expr: Box<Expr>,
        span: Span,
    },
    Ident {
        name: String,
        span: Span,
    },
    If {
        condition: Box<Expr>,
        then_body: Vec<Statement>,
        else_body: Option<Vec<Statement>>,
        span: Span,
    },
    List {
        elements: Vec<Expr>,
        span: Span,
    },
    Literal {
        value: Literal,
        span: Span,
    },
    Loop {
        body: Vec<Statement>,
        span: Span,
    },
    Match {
        subject: Box<Expr>,
        arms: Vec<MatchArm>,
        span: Span,
    },
    MethodCall {
        receiver: Box<Expr>,
        method: String,
        type_args: Option<Vec<TypeExpr>>,
        args: Vec<Arg>,
        span: Span,
    },
    Receive {
        arms: Vec<ReceiveArm>,
        span: Span,
    },
    Self_ {
        span: Span,
    },
    ShortClosure {
        params: Vec<ClosureParam>,
        body: Box<Expr>,
        span: Span,
    },
    Spawn {
        expr: Box<Expr>,
        span: Span,
    },
    String {
        parts: Vec<StringPart>,
        multiline: bool,
        span: Span,
    },
    StructConstruction {
        type_path: Vec<String>,
        fields: Vec<FieldInit>,
        span: Span,
    },
    Ternary {
        condition: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
        span: Span,
    },
    Try {
        expr: Box<Expr>,
        span: Span,
    },
    Tuple {
        elements: Vec<Expr>,
        span: Span,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
        span: Span,
    },
    Unless {
        condition: Box<Expr>,
        body: Vec<Statement>,
        span: Span,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    And,
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
    Pipe,
    Sub,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone)]
pub enum Literal {
    Bool(bool),
    Float(String),
    Int(String),
    None,
    Unit,
}

#[derive(Debug, Clone)]
pub struct Arg {
    pub name: Option<String>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FieldInit {
    pub name: String,
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum StringPart {
    Literal {
        value: String,
        span: Span,
    },
    Interpolation {
        expr: Expr,
        format: Option<String>,
        span: Span,
    },
}

#[derive(Debug, Clone)]
pub enum EnumConstructionData {
    Unit,
    Tuple(Vec<Expr>),
    Struct(Vec<FieldInit>),
}

#[derive(Debug, Clone)]
pub enum ClosureParam {
    Name { name: String, span: Span },
    Destructured { names: Vec<String>, span: Span },
    Wildcard { span: Span },
}

// ============================================================================
// Match / cond / receive arms
// ============================================================================

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Vec<Statement>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct CondArm {
    pub condition: Expr,
    pub body: Vec<Statement>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ReceiveArm {
    pub pattern: Pattern,
    pub source: Expr,
    pub body: Vec<Statement>,
    pub span: Span,
}

// ============================================================================
// Patterns
// ============================================================================

#[derive(Debug, Clone)]
pub enum Pattern {
    Wildcard {
        span: Span,
    },
    Literal {
        value: Literal,
        span: Span,
    },
    Binding {
        name: String,
        span: Span,
    },
    EnumUnit {
        type_path: Vec<String>,
        variant: String,
        span: Span,
    },
    EnumTuple {
        type_path: Vec<String>,
        variant: String,
        elements: Vec<Pattern>,
        span: Span,
    },
    EnumStruct {
        type_path: Vec<String>,
        variant: String,
        fields: Vec<FieldPattern>,
        span: Span,
    },
    /// Shorthand constructors: Some(x), Ok(x), Err(x)
    Constructor {
        name: String,
        elements: Vec<Pattern>,
        span: Span,
    },
    Tuple {
        elements: Vec<Pattern>,
        span: Span,
    },
    List {
        elements: Vec<Pattern>,
        span: Span,
    },
}

#[derive(Debug, Clone)]
pub struct FieldPattern {
    pub name: String,
    pub pattern: Option<Pattern>,
    pub span: Span,
}
