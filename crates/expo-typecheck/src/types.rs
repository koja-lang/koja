use expo_ast::ast::TypeExpr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Enum(String),
    Error,
    Function {
        params: Vec<Type>,
        return_type: Box<Type>,
    },
    Primitive(Primitive),
    Struct(String),
    Tuple(Vec<Type>),
    Unit,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Primitive {
    Bool,
    F32,
    F64,
    I8,
    I16,
    I32,
    I64,
    String,
    U8,
    U16,
    U32,
    U64,
}

impl Type {
    pub fn display(&self) -> String {
        match self {
            Type::Enum(name) => name.clone(),
            Type::Error => "<error>".to_string(),
            Type::Function {
                params,
                return_type,
            } => {
                let p: Vec<String> = params.iter().map(|t| t.display()).collect();
                format!("({}) -> {}", p.join(", "), return_type.display())
            }
            Type::Primitive(p) => p.display().to_string(),
            Type::Struct(name) => name.clone(),
            Type::Tuple(elems) => {
                let inner: Vec<String> = elems.iter().map(|t| t.display()).collect();
                format!("({})", inner.join(", "))
            }
            Type::Unit => "()".to_string(),
            Type::Unknown => "unknown".to_string(),
        }
    }

    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            Type::Primitive(
                Primitive::F32
                    | Primitive::F64
                    | Primitive::I8
                    | Primitive::I16
                    | Primitive::I32
                    | Primitive::I64
                    | Primitive::U8
                    | Primitive::U16
                    | Primitive::U32
                    | Primitive::U64
            )
        )
    }
}

impl Primitive {
    pub fn display(&self) -> &'static str {
        match self {
            Primitive::Bool => "bool",
            Primitive::F32 => "f32",
            Primitive::F64 => "f64",
            Primitive::I8 => "i8",
            Primitive::I16 => "i16",
            Primitive::I32 => "i32",
            Primitive::I64 => "i64",
            Primitive::String => "string",
            Primitive::U8 => "u8",
            Primitive::U16 => "u16",
            Primitive::U32 => "u32",
            Primitive::U64 => "u64",
        }
    }
}

pub fn resolve_type_expr(
    type_expr: &TypeExpr,
    known_structs: &[&str],
    known_enums: &[&str],
) -> Type {
    match type_expr {
        TypeExpr::Generic { .. } | TypeExpr::Ref { .. } => Type::Unknown,
        TypeExpr::Named { path, .. } => {
            if path.len() == 1 {
                match path[0].as_str() {
                    "string" => Type::Primitive(Primitive::String),
                    "bool" => Type::Primitive(Primitive::Bool),
                    "f32" => Type::Primitive(Primitive::F32),
                    "f64" => Type::Primitive(Primitive::F64),
                    "i8" => Type::Primitive(Primitive::I8),
                    "i16" => Type::Primitive(Primitive::I16),
                    "i32" => Type::Primitive(Primitive::I32),
                    "i64" => Type::Primitive(Primitive::I64),
                    "u8" => Type::Primitive(Primitive::U8),
                    "u16" => Type::Primitive(Primitive::U16),
                    "u32" => Type::Primitive(Primitive::U32),
                    "u64" => Type::Primitive(Primitive::U64),
                    name => {
                        if known_structs.contains(&name) {
                            Type::Struct(name.to_string())
                        } else if known_enums.contains(&name) {
                            Type::Enum(name.to_string())
                        } else {
                            Type::Unknown
                        }
                    }
                }
            } else {
                Type::Unknown
            }
        }
        TypeExpr::Tuple { elements, .. } => {
            let types: Vec<Type> = elements
                .iter()
                .map(|e| resolve_type_expr(e, known_structs, known_enums))
                .collect();
            Type::Tuple(types)
        }
        TypeExpr::Unit { .. } => Type::Unit,
    }
}
