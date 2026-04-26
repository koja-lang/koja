//! [`IRProgram`]: the first concrete IR-level container, collecting all
//! monomorphized declarations a backend needs to emit. Populated by the
//! `monomorphize_*` planners in [`crate::lower::monomorphize`] and consumed
//! by emission-side code (today: `expo-codegen`).
//!
//! At this slice, declarations are at the **declaration level only**:
//! struct field layouts, enum variant payloads, and function signatures
//! are concrete `Type`s, but function bodies are still raw AST
//! ([`expo_ast::ast::Function`]) — bottom-up IR-ification of bodies into
//! basic blocks and instructions is the next wave's work. See
//! `expo/design/EXPOIR.md`.
//!
//! Long-term, [`IRProgram`] is expected to grow into a thin container of
//! `IRPackage`s so users can address per-package partitioning. For now the
//! flat shape mirrors codegen's `Compiler.functions` /
//! `LLVMTypeCache.monomorphized` and slots cleanly into the existing
//! shim-based migration.

use std::collections::HashMap;

use expo_ast::ast::Function;
use expo_typecheck::context::VariantData;
use expo_typecheck::types::Type;

use crate::identity::{FunctionIdentifier, MonomorphizedTypeIdentifier};

/// Top-level IR container: a flat collection of monomorphized struct,
/// enum, and function declarations awaiting backend emission.
///
/// Insertion order is preserved via the `*_order` vectors so emission can
/// walk decls in dependency-stable order (matches the implicit ordering
/// the previous monomorphization-during-emission produced).
#[derive(Default)]
pub struct IRProgram {
    pub structs: HashMap<MonomorphizedTypeIdentifier, IRStruct>,
    pub struct_order: Vec<MonomorphizedTypeIdentifier>,
    pub enums: HashMap<MonomorphizedTypeIdentifier, IREnum>,
    pub enum_order: Vec<MonomorphizedTypeIdentifier>,
    pub functions: HashMap<FunctionIdentifier, IRFunction>,
    pub function_order: Vec<FunctionIdentifier>,
}

impl IRProgram {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn contains_struct(&self, id: &MonomorphizedTypeIdentifier) -> bool {
        self.structs.contains_key(id)
    }

    pub fn contains_enum(&self, id: &MonomorphizedTypeIdentifier) -> bool {
        self.enums.contains_key(id)
    }

    pub fn contains_function(&self, id: &FunctionIdentifier) -> bool {
        self.functions.contains_key(id)
    }

    /// Inserts a struct decl and records its position in `struct_order`.
    /// Idempotent on the order list: re-inserting an existing key replaces
    /// the decl but does not duplicate the order entry.
    pub fn insert_struct(&mut self, decl: IRStruct) {
        let id = decl.mangled.clone();
        if !self.structs.contains_key(&id) {
            self.struct_order.push(id.clone());
        }
        self.structs.insert(id, decl);
    }

    pub fn insert_enum(&mut self, decl: IREnum) {
        let id = decl.mangled.clone();
        if !self.enums.contains_key(&id) {
            self.enum_order.push(id.clone());
        }
        self.enums.insert(id, decl);
    }

    pub fn insert_function(&mut self, decl: IRFunction) {
        let id = decl.mangled.clone();
        if !self.functions.contains_key(&id) {
            self.function_order.push(id.clone());
        }
        self.functions.insert(id, decl);
    }
}

/// A monomorphized struct declaration with concrete field types.
///
/// `kind` distinguishes user-defined structs (whose layout is computed
/// from the resolved `fields`) from stdlib intrinsic structs whose
/// physical layout is hard-coded by the backend (e.g. `List<T>` is
/// `{ ptr, length, capacity }` regardless of `T`). Backends may use
/// `kind` to short-circuit field-driven layout in favor of an intrinsic
/// emitter; the resolved `fields` are still populated for consistency
/// and consumption by future passes.
#[derive(Clone)]
pub struct IRStruct {
    pub mangled: MonomorphizedTypeIdentifier,
    pub fields: Vec<(String, Type)>,
    pub kind: IRStructKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IRStructKind {
    /// User-defined struct; layout is derived from `fields`.
    User,
    /// `std.List<T>` — layout `{ ptr: i8*, length: i64, capacity: i64 }`.
    StdList,
    /// `std.Map<K,V>` or `std.Set<T>` — shared hashtable layout.
    StdHashtable,
    /// `std.Ref<T>` — single owning pointer.
    StdRef,
    /// `std.ReplyTo<T>` — process-reply channel handle.
    StdReplyTo,
}

/// A monomorphized enum declaration with concrete variant payloads.
#[derive(Clone)]
pub struct IREnum {
    pub mangled: MonomorphizedTypeIdentifier,
    pub variants: Vec<(String, VariantData)>,
}

/// A monomorphized function or method declaration.
///
/// The body is held as raw AST in this slice; future waves replace
/// `func_ast` with explicit IR basic blocks and instructions.
#[derive(Clone)]
pub struct IRFunction {
    pub mangled: FunctionIdentifier,
    pub func_ast: Function,
    pub param_types: Vec<Type>,
    pub return_type: Type,
    pub subst: HashMap<String, Type>,
    pub kind: IRFunctionKind,
}

/// Distinguishes free functions from impl methods. Methods carry the
/// extra context the backend needs to emit a `self`-bearing signature.
#[derive(Clone)]
pub enum IRFunctionKind {
    /// Free function (top-level, no `self`).
    Free,
    /// Impl method (instance or static).
    Method {
        /// Unmangled base type (e.g. `"List"`, `"MyStruct"`).
        base_type: String,
        /// Mangled `self`-type identifier (e.g. `"List_$Int32$"`).
        mangled_type: MonomorphizedTypeIdentifier,
        /// `Some(t)` for instance methods (the `self` parameter type),
        /// `None` for static methods.
        self_type: Option<Type>,
        /// Whether this method has no `self` (static dispatch).
        is_static: bool,
    },
}
