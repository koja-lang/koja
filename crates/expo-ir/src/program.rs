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
//! `expo/design/EXPOIR-ROADMAP.md`.
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

/// A callable symbol declaration.
///
/// `Free` and `Method` carry an Expo AST body emitted by codegen;
/// `Extern` denotes a foreign declaration whose body lives outside the
/// Expo source and must be resolved by the linker; `Intrinsic` and
/// `Thunk` carry hand-emitted bodies dispatched by the backend;
/// `MainEntry` tags the transitional `fn main` synthesis pair. Future
/// waves replace the AST bodies on `Free` / `Method` with explicit IR
/// basic blocks and instructions.
#[derive(Clone)]
pub struct IRFunction {
    pub mangled: FunctionIdentifier,
    pub param_types: Vec<Type>,
    pub return_type: Type,
    pub kind: IRFunctionKind,
}

/// ABI of a foreign-linked symbol. A single-variant enum today; future
/// ABIs (`System`, `RustRuntime`, ...) drop in without breaking the
/// `Extern` shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExternAbi {
    C,
}

/// Backend-actionable attributes for a foreign-linked symbol. Captures
/// everything a backend needs to declare and link the symbol without
/// consulting the LLVM module: the calling convention, an optional
/// override of the symbol name (`@link "lib:symbol"`), the library to
/// pass to the linker (`@link "lib"`), and whether the C signature is
/// variadic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternAttrs {
    pub abi: ExternAbi,
    pub is_variadic: bool,
    pub link_lib: Option<String>,
    pub link_name: Option<String>,
}

impl ExternAttrs {
    /// Default attributes for a hand-declared C ABI symbol with no
    /// link overrides (e.g. libc / Expo runtime functions registered
    /// from `builtins.rs`).
    pub fn c(is_variadic: bool) -> Self {
        Self {
            abi: ExternAbi::C,
            is_variadic,
            link_lib: None,
            link_name: None,
        }
    }
}

/// Discriminates the callable symbol categories tracked by
/// [`IRProgram`]. `Free` and `Method` own the AST body codegen lowers
/// to LLVM; `Extern` is a linker-resolved declaration; `Intrinsic`,
/// `MainEntry`, and `Thunk` carry no AST body because the
/// implementation is hand-emitted by the backend.
#[derive(Clone)]
pub enum IRFunctionKind {
    /// Foreign-linked symbol with no AST body. Covers C stdlib FFI
    /// (`printf`, `malloc`, ...), Expo runtime FFI (`expo_rt_*`,
    /// `expo_string_*`, ...), and user-source `@extern "C"`
    /// declarations. The carried [`ExternAttrs`] is sufficient for any
    /// backend to declare and link the symbol without consulting the
    /// LLVM module.
    Extern(ExternAttrs),
    /// Free function (top-level, no `self`).
    Free {
        func_ast: Function,
        subst: HashMap<String, Type>,
    },
    /// Compiler-defined method whose body is hand-emitted by the
    /// backend (no AST). Originally introduced for stdlib types
    /// (`List.append`, `Map.get`, `CPtr.read`, ...) and now also
    /// covers compiler-synthesized per-type methods like the
    /// `inspect` / `format` helpers in `expo-codegen::debug`.
    /// `(base_type, method_name)` is the minimum identity a backend
    /// needs to dispatch its own implementation.
    Intrinsic {
        /// Unmangled base type the method belongs to (e.g. `"List"`,
        /// `"Int"`, or a user struct's bare name).
        base_type: String,
        /// Method name as written in the source (e.g. `"append"`,
        /// `"inspect"`, `"format"`).
        method_name: String,
    },
    /// Compiler-synthesized entry-point pair for the legacy `fn main`
    /// convention: the LLVM `main` C entry that calls
    /// `expo_rt_spawn(__expo_user_main, ...)`, and `__expo_user_main`
    /// itself which holds the user-written body.
    ///
    /// Transitional: `fn main` is slated for retirement and the
    /// replacement entry-point convention will get its own
    /// classification at that time.
    MainEntry,
    /// Impl method (instance or static).
    Method {
        func_ast: Function,
        subst: HashMap<String, Type>,
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
    /// Forwarding wrapper that adapts a top-level function for use as
    /// a closure-compatible fat pointer. The body is synthetic
    /// (forward-call to `wraps`), generated by the backend on demand.
    Thunk {
        /// The underlying function this thunk forwards to.
        wraps: FunctionIdentifier,
    },
}
