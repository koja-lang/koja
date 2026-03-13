use expo_ast::ast::TypeExpr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Primitive(Primitive),
    Unit,
    Struct(String),
    Tuple(Vec<Type>),
    Unknown,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Primitive {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
    Bool,
    String,
}

impl Type {
    pub fn display(&self) -> String {
        match self {
            Type::Primitive(p) => p.display().to_string(),
            Type::Unit => "()".to_string(),
            Type::Struct(name) => name.clone(),
            Type::Tuple(elems) => {
                let inner: Vec<String> = elems.iter().map(|t| t.display()).collect();
                format!("({})", inner.join(", "))
            }
            Type::Unknown => "unknown".to_string(),
            Type::Error => "<error>".to_string(),
        }
    }

    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            Type::Primitive(
                Primitive::I8
                    | Primitive::I16
                    | Primitive::I32
                    | Primitive::I64
                    | Primitive::U8
                    | Primitive::U16
                    | Primitive::U32
                    | Primitive::U64
                    | Primitive::F32
                    | Primitive::F64
            )
        )
    }
}

impl Primitive {
    pub fn display(&self) -> &'static str {
        match self {
            Primitive::I8 => "i8",
            Primitive::I16 => "i16",
            Primitive::I32 => "i32",
            Primitive::I64 => "i64",
            Primitive::U8 => "u8",
            Primitive::U16 => "u16",
            Primitive::U32 => "u32",
            Primitive::U64 => "u64",
            Primitive::F32 => "f32",
            Primitive::F64 => "f64",
            Primitive::Bool => "bool",
            Primitive::String => "String",
        }
    }
}

pub fn resolve_type_expr(type_expr: &TypeExpr, known_structs: &[&str]) -> Type {
    match type_expr {
        TypeExpr::Named { path, .. } => {
            if path.len() == 1 {
                match path[0].as_str() {
                    "i8" => Type::Primitive(Primitive::I8),
                    "i16" => Type::Primitive(Primitive::I16),
                    "i32" => Type::Primitive(Primitive::I32),
                    "i64" => Type::Primitive(Primitive::I64),
                    "u8" => Type::Primitive(Primitive::U8),
                    "u16" => Type::Primitive(Primitive::U16),
                    "u32" => Type::Primitive(Primitive::U32),
                    "u64" => Type::Primitive(Primitive::U64),
                    "f32" => Type::Primitive(Primitive::F32),
                    "f64" => Type::Primitive(Primitive::F64),
                    "bool" => Type::Primitive(Primitive::Bool),
                    "String" => Type::Primitive(Primitive::String),
                    name => {
                        if known_structs.contains(&name) {
                            Type::Struct(name.to_string())
                        } else {
                            Type::Unknown
                        }
                    }
                }
            } else {
                Type::Unknown
            }
        }
        TypeExpr::Unit { .. } => Type::Unit,
        TypeExpr::Tuple { elements, .. } => {
            let types: Vec<Type> = elements
                .iter()
                .map(|e| resolve_type_expr(e, known_structs))
                .collect();
            Type::Tuple(types)
        }
        TypeExpr::Generic { .. } | TypeExpr::Ref { .. } => Type::Unknown,
    }
}
