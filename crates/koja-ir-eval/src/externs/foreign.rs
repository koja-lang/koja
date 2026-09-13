//! The second tier of `@extern "C"` dispatch. Symbols with no shim in
//! [`super::dispatch`] resolve through `dlopen` / `dlsym` and run
//! through libffi with the signature the declaration carries.
//!
//! Resolution happens once per run, before the first instruction,
//! so the driver can settle the backend on the result. A program
//! whose every extern resolves runs on the interpreter, and one with
//! a symbol the loader cannot find compiles natively instead. A
//! declared extern that never resolves is still callable in the
//! sense that the call raises [`RuntimeError::ExternUnresolved`]
//! with the loader's reason, so declaring one costs nothing until
//! the call.
//!
//! Lookup order for one symbol, C name `c_name` with `@link "lib"`:
//!
//! 1. `lib{lib}.dylib` or `lib{lib}.so` under each search path the
//!    caller supplies (the project root, the script's directory).
//! 2. The same file name bare, so the loader applies its own search
//!    (`DYLD_LIBRARY_PATH`, `LD_LIBRARY_PATH`, the system paths).
//! 3. The running process, `dlsym(RTLD_DEFAULT, c_name)`. This covers
//!    libc and libm, which the `koja` binary already links, and it is
//!    the path that works on Linux where `libm.so` is a linker script
//!    that `dlopen` rejects.
//!
//! A static archive is never loadable here. The driver's fallback to
//! LLVM covers a project that ships only a `.a`.
//!
//! The extern surface the typechecker admits is explicit-width
//! integers, `Bool`, `Float32`, `Float64`, `CPtr<T>`, and `()`. Every
//! one is a libffi primitive, so a `Cif` per function covers the
//! whole surface with no struct or variadic handling. A call blocks
//! the interpreter's scheduler for its duration, the same way it
//! blocks one worker natively.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::fmt;
use std::path::Path;
use std::rc::Rc;

use koja_ir::panics::extern_non_finite_message;
use koja_ir::{FunctionKind, IRFunction, IRPackage, IRType};
use libffi::middle::{Arg, Cif, CodePtr, Type, arg};

use crate::error::RuntimeError;
use crate::value::Value;

/// One `@extern "C"` symbol the loader could not find.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unresolved {
    /// The C symbol looked up (`@link "lib:name"` or the function name).
    pub c_name: String,
    /// The `@link` library, when the declaration names one.
    pub link_lib: Option<String>,
    /// The loader's message for the last lookup that failed.
    pub reason: String,
    /// The mangled Koja symbol of the declaring function.
    pub symbol: String,
}

impl Unresolved {
    fn into_error(self) -> RuntimeError {
        RuntimeError::ExternUnresolved {
            c_name: self.c_name,
            link_lib: self.link_lib,
            reason: self.reason,
            symbol: self.symbol,
        }
    }
}

impl fmt::Display for Unresolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "extern \"C\" `{}`", self.c_name)?;
        if let Some(lib) = &self.link_lib {
            write!(f, " (@link \"{lib}\")")?;
        }
        write!(f, " could not be resolved: {}", self.reason)
    }
}

/// A resolved foreign function, its address plus the libffi call
/// interface built from the declared parameter and return types.
pub struct ForeignFunction {
    c_name: String,
    cif: Cif,
    code: CodePtr,
    params: Vec<IRType>,
    ret: IRType,
}

/// Every foreign extern in a program, keyed by mangled Koja symbol,
/// resolved or not. Holds the loaded libraries so the addresses stay
/// valid for the run.
#[derive(Default)]
pub struct ForeignTable {
    functions: HashMap<String, Result<ForeignFunction, Unresolved>>,
    /// Kept alive for the addresses in `functions`. Never read.
    _libraries: Vec<libloading::Library>,
}

impl ForeignTable {
    /// Resolves every `@extern "C"` function in `packages` that has
    /// no shim in the dispatch table. `search_paths` are tried first
    /// for each `@link` library, in order.
    pub fn resolve(packages: &[IRPackage], search_paths: &[&Path]) -> ForeignTable {
        let mut loader = Loader::new(search_paths);
        let mut functions = HashMap::new();
        for function in packages
            .iter()
            .flat_map(|package| package.functions.values())
        {
            let FunctionKind::Extern(attrs) = &function.kind else {
                continue;
            };
            let c_name = attrs
                .link_name
                .as_deref()
                .unwrap_or_else(|| function.symbol.last_segment());
            if super::SUPPORTED_EXTERNS.binary_search(&c_name).is_ok() {
                continue;
            }
            let entry = match loader.lookup(c_name, attrs.link_lib.as_deref()) {
                Ok(code) => Ok(ForeignFunction::new(c_name, code, function)),
                Err(reason) => Err(Unresolved {
                    c_name: c_name.to_string(),
                    link_lib: attrs.link_lib.clone(),
                    reason,
                    symbol: function.symbol.mangled().to_string(),
                }),
            };
            functions.insert(function.symbol.mangled().to_string(), entry);
        }
        ForeignTable {
            functions,
            _libraries: loader.into_libraries(),
        }
    }

    /// The symbols the loader could not find, in symbol order.
    pub fn unresolved(&self) -> Vec<&Unresolved> {
        let mut missing: Vec<&Unresolved> = self
            .functions
            .values()
            .filter_map(|entry| entry.as_ref().err())
            .collect();
        missing.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        missing
    }

    /// Calls the foreign function behind `function`, or `None` when
    /// the table has no entry for it.
    fn call(&self, function: &IRFunction, args: &[Value]) -> Option<Result<Value, RuntimeError>> {
        let entry = self.functions.get(function.symbol.mangled())?;
        Some(match entry {
            Ok(foreign) => foreign.call(args),
            Err(unresolved) => Err(unresolved.clone().into_error()),
        })
    }
}

/// Opens `@link` libraries on demand and remembers each handle.
struct Loader<'a> {
    libraries: Vec<libloading::Library>,
    /// Index into `libraries` per `@link` name, or the open error.
    opened: HashMap<String, Result<usize, String>>,
    search_paths: &'a [&'a Path],
}

impl<'a> Loader<'a> {
    fn new(search_paths: &'a [&'a Path]) -> Self {
        Loader {
            libraries: Vec::new(),
            opened: HashMap::new(),
            search_paths,
        }
    }

    fn into_libraries(self) -> Vec<libloading::Library> {
        self.libraries
    }

    /// Finds `c_name` through its `@link` library first, then in the
    /// running process.
    fn lookup(&mut self, c_name: &str, link_lib: Option<&str>) -> Result<CodePtr, String> {
        let mut failure = None;
        if let Some(lib) = link_lib {
            match self.open(lib) {
                Ok(index) => match symbol(&self.libraries[index], c_name) {
                    Ok(code) => return Ok(code),
                    Err(err) => failure = Some(format!("`{}`: {err}", library_file(lib))),
                },
                Err(err) => failure = Some(err),
            }
        }
        // `Library::this()` is the process itself, so this is
        // `dlsym(RTLD_DEFAULT, c_name)`.
        let process = libloading::Library::from(libloading::os::unix::Library::this());
        match symbol(&process, c_name) {
            Ok(code) => Ok(code),
            Err(err) => Err(match failure {
                Some(first) => format!("{first}, and the running process: {err}"),
                None => format!("the running process: {err}"),
            }),
        }
    }

    /// Opens `lib` once, trying each search path and then the bare
    /// file name.
    fn open(&mut self, lib: &str) -> Result<usize, String> {
        if let Some(cached) = self.opened.get(lib) {
            return cached.clone();
        }
        let file = library_file(lib);
        let mut candidates: Vec<std::path::PathBuf> = self
            .search_paths
            .iter()
            .map(|dir| dir.join(&file))
            .collect();
        candidates.push(std::path::PathBuf::from(&file));
        let mut last_error = String::new();
        for candidate in &candidates {
            // SAFETY: loading a library runs its initializers, which is
            // the documented contract of `@link`. The caller declared
            // the library and asked for it to be loaded.
            match unsafe { libloading::Library::new(candidate) } {
                Ok(library) => {
                    self.libraries.push(library);
                    let index = self.libraries.len() - 1;
                    self.opened.insert(lib.to_string(), Ok(index));
                    return Ok(index);
                }
                Err(err) => last_error = err.to_string(),
            }
        }
        let outcome = Err(format!(
            "`{file}` is not a shared library on the search path ({last_error})"
        ));
        self.opened.insert(lib.to_string(), outcome.clone());
        outcome
    }
}

/// The platform file name for `@link "lib"`, `libm.dylib` or `libm.so`.
fn library_file(lib: &str) -> String {
    libloading::library_filename(lib)
        .to_string_lossy()
        .into_owned()
}

fn symbol(library: &libloading::Library, c_name: &str) -> Result<CodePtr, String> {
    let name = format!("{c_name}\0");
    // SAFETY: the symbol is only ever called through a `Cif` built
    // from the Koja declaration, which the typechecker checked
    // against the FFI-admissible surface. Its C type is the
    // declarer's promise, as it is for the LLVM backend.
    let found = unsafe { library.get::<*const c_void>(name.as_bytes()) };
    match found {
        Ok(address) => Ok(CodePtr::from_ptr(*address)),
        Err(err) => Err(err.to_string()),
    }
}

/// One marshaled argument, stored at its declared C width so libffi
/// reads the right number of bytes.
enum Slot {
    F32(f32),
    F64(f64),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    Ptr(*mut u8),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
}

impl Slot {
    fn as_arg(&self) -> Arg<'_> {
        match self {
            Slot::F32(v) => arg(v),
            Slot::F64(v) => arg(v),
            Slot::I8(v) => arg(v),
            Slot::I16(v) => arg(v),
            Slot::I32(v) => arg(v),
            Slot::I64(v) => arg(v),
            Slot::Ptr(v) => arg(v),
            Slot::U8(v) => arg(v),
            Slot::U16(v) => arg(v),
            Slot::U32(v) => arg(v),
            Slot::U64(v) => arg(v),
        }
    }
}

impl ForeignFunction {
    fn new(c_name: &str, code: CodePtr, function: &IRFunction) -> Self {
        let params: Vec<IRType> = function.params.iter().map(|p| p.ty.clone()).collect();
        let cif = Cif::new(params.iter().map(ffi_type), ffi_type(&function.return_type));
        ForeignFunction {
            c_name: c_name.to_string(),
            cif,
            code,
            params,
            ret: function.return_type.clone(),
        }
    }

    /// Runs the call. Integer returns narrower than 64 bits come back
    /// from libffi widened to a register, so they are truncated to
    /// the declared width and extended by its signedness. A
    /// non-finite float return is the same panic LLVM emits.
    fn call(&self, args: &[Value]) -> Result<Value, RuntimeError> {
        let slots: Vec<Slot> = self
            .params
            .iter()
            .zip(args)
            .map(|(ty, value)| marshal(&self.c_name, ty, value))
            .collect::<Result<_, _>>()?;
        let ffi_args: Vec<Arg> = slots.iter().map(Slot::as_arg).collect();
        // SAFETY: `cif` describes exactly the parameter and return
        // types the Koja declaration carries, `ffi_args` holds one
        // storage slot per parameter at that width, and the slots
        // outlive the call. The callee's real C type is the
        // declarer's promise, the same contract the LLVM backend
        // links under.
        unsafe {
            match &self.ret {
                IRType::Unit => {
                    self.cif.call::<()>(self.code, &ffi_args);
                    Ok(Value::Unit)
                }
                IRType::Float32 => {
                    let v = self.cif.call::<f32>(self.code, &ffi_args);
                    self.finite(f64::from(v)).map(|()| Value::Float32(v))
                }
                IRType::Float64 => {
                    let v = self.cif.call::<f64>(self.code, &ffi_args);
                    self.finite(v).map(|()| Value::Float64(v))
                }
                IRType::CPtr(_) => {
                    let v = self.cif.call::<*mut u8>(self.code, &ffi_args);
                    Ok(Value::CPtr(v))
                }
                IRType::Bool => {
                    let v = self.cif.call::<Register>(self.code, &ffi_args);
                    Ok(Value::Bool(v as u8 != 0))
                }
                ty => {
                    let v = self.cif.call::<Register>(self.code, &ffi_args);
                    Ok(Value::Int(narrow(ty, v)))
                }
            }
        }
    }

    fn finite(&self, value: f64) -> Result<(), RuntimeError> {
        if value.is_finite() {
            Ok(())
        } else {
            Err(RuntimeError::Panicked {
                message: extern_non_finite_message(&self.c_name),
            })
        }
    }
}

/// The buffer libffi fills for an integer return. libffi widens every
/// integer narrower than a register into an `ffi_arg`, so the buffer
/// must be register sized whatever the declared width. `u64` is that
/// size on every supported target, which the assert pins.
type Register = u64;

const _: () = assert!(
    std::mem::size_of::<libffi::raw::ffi_arg>() == std::mem::size_of::<Register>(),
    "foreign calls assume a 64-bit ffi_arg"
);

/// Truncates a register-widened integer return to `ty` and extends it
/// back to the interpreter's `i64` by the type's signedness.
fn narrow(ty: &IRType, v: Register) -> i64 {
    match ty {
        IRType::Int8 => v as i8 as i64,
        IRType::Int16 => v as i16 as i64,
        IRType::Int32 => v as i32 as i64,
        IRType::UInt8 => v as u8 as i64,
        IRType::UInt16 => v as u16 as i64,
        IRType::UInt32 => v as u32 as i64,
        _ => v as i64,
    }
}

fn ffi_type(ty: &IRType) -> Type {
    match ty {
        IRType::Bool => Type::u8(),
        IRType::CPtr(_) => Type::pointer(),
        IRType::Float32 => Type::f32(),
        IRType::Float64 => Type::f64(),
        IRType::Int8 => Type::i8(),
        IRType::Int16 => Type::i16(),
        IRType::Int32 => Type::i32(),
        IRType::Int64 => Type::i64(),
        IRType::UInt8 => Type::u8(),
        IRType::UInt16 => Type::u16(),
        IRType::UInt32 => Type::u32(),
        IRType::UInt64 => Type::u64(),
        IRType::Unit => Type::void(),
        other => unreachable!("typecheck admits no `{other:?}` at an extern boundary"),
    }
}

fn marshal(c_name: &str, ty: &IRType, value: &Value) -> Result<Slot, RuntimeError> {
    let mismatch = || RuntimeError::TypeMismatch {
        detail: format!("{c_name} expects `{ty:?}`, got {value:?}"),
    };
    Ok(match (ty, value) {
        (IRType::Bool, Value::Bool(b)) => Slot::U8(u8::from(*b)),
        (IRType::CPtr(_), Value::CPtr(p)) => Slot::Ptr(*p),
        (IRType::Float32, Value::Float32(f)) => Slot::F32(*f),
        (IRType::Float64, Value::Float64(f)) => Slot::F64(*f),
        (IRType::Int8, Value::Int(i)) => Slot::I8(*i as i8),
        (IRType::Int16, Value::Int(i)) => Slot::I16(*i as i16),
        (IRType::Int32, Value::Int(i)) => Slot::I32(*i as i32),
        (IRType::Int64, Value::Int(i)) => Slot::I64(*i),
        (IRType::UInt8, Value::Int(i)) => Slot::U8(*i as u8),
        (IRType::UInt16, Value::Int(i)) => Slot::U16(*i as u16),
        (IRType::UInt32, Value::Int(i)) => Slot::U32(*i as u32),
        (IRType::UInt64, Value::Int(i)) => Slot::U64(*i as u64),
        _ => return Err(mismatch()),
    })
}

thread_local! {
    /// The table for the in-flight run, installed beside the
    /// scheduler's runtime and cleared with it.
    static FOREIGN: RefCell<Option<Rc<ForeignTable>>> = const { RefCell::new(None) };
}

/// Clears the installed table on drop.
pub(crate) struct ForeignGuard;

impl Drop for ForeignGuard {
    fn drop(&mut self) {
        FOREIGN.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Installs `table` for the current run. The guard removes it.
pub(crate) fn install(table: ForeignTable) -> ForeignGuard {
    FOREIGN.with(|slot| *slot.borrow_mut() = Some(Rc::new(table)));
    ForeignGuard
}

/// Calls `function` through the installed table, or `None` when no
/// table is installed or it has no entry for the symbol.
pub(crate) fn call(function: &IRFunction, args: &[Value]) -> Option<Result<Value, RuntimeError>> {
    let table = FOREIGN.with(|slot| slot.borrow().clone())?;
    table.call(function, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_extends_by_declared_signedness() {
        // A callee returning `(int8_t)-1` leaves `0xff` in the low
        // byte and whatever the ABI leaves above it.
        let register: Register = 0xffff_ffff_ffff_ffff;
        assert_eq!(narrow(&IRType::Int8, register), -1);
        assert_eq!(narrow(&IRType::UInt8, register), 0xff);
        assert_eq!(narrow(&IRType::Int16, register), -1);
        assert_eq!(narrow(&IRType::UInt16, register), 0xffff);
        assert_eq!(narrow(&IRType::Int32, register), -1);
        assert_eq!(narrow(&IRType::UInt32, register), 0xffff_ffff);
        assert_eq!(narrow(&IRType::Int64, register), -1);

        let garbage_above: Register = 0x1234_5678_0000_0080;
        assert_eq!(narrow(&IRType::Int8, garbage_above), -128);
        assert_eq!(narrow(&IRType::UInt8, garbage_above), 128);
    }

    #[test]
    fn unresolved_display_names_symbol_library_and_reason() {
        let missing = Unresolved {
            c_name: "ffi_helper_add".to_string(),
            link_lib: Some("ffi_helper".to_string()),
            reason: "`libffi_helper.so` is not a shared library on the search path".to_string(),
            symbol: "Main.ffi_helper_add/2".to_string(),
        };
        assert_eq!(
            missing.to_string(),
            "extern \"C\" `ffi_helper_add` (@link \"ffi_helper\") could not be resolved: \
             `libffi_helper.so` is not a shared library on the search path",
        );

        let bare = Unresolved {
            link_lib: None,
            ..missing
        };
        assert_eq!(
            bare.to_string(),
            "extern \"C\" `ffi_helper_add` could not be resolved: \
             `libffi_helper.so` is not a shared library on the search path",
        );
    }

    #[test]
    fn library_file_uses_the_platform_prefix_and_suffix() {
        let file = library_file("m");
        assert!(file.starts_with("libm."), "got `{file}`");
        assert!(
            file.ends_with(".dylib") || file.ends_with(".so"),
            "got `{file}`"
        );
    }
}
