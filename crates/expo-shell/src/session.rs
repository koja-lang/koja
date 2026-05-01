//! Accumulating REPL session state and the per-input re-execution
//! pipeline. Lives in its own module so [`crate`] only owns the
//! stdin loop and the file-eval entry point.
//!
//! See the crate-level module docs for the re-execution model and
//! its scaling caveats.

use std::path::PathBuf;
use std::sync::Arc;

use expo_ast::ast::{Item, Module, Statement};
use expo_ir::{Backend, FunctionIdentifier};
use expo_ir_eval::{Interp, Value};
use expo_typecheck::types::Type;

use crate::{format_diagnostics, parse_module};

/// Synthetic name the [`Session`] re-execution loop wraps the
/// accumulated statement blocks in, so the interpreter has a callable
/// entry point with a known return type. Reserved -- user code
/// shouldn't shadow it.
const SYNTHETIC_SESSION_ENTRY: &str = "__expo_session_main__";

/// Synthetic package name used for both typecheck and codegen of the
/// session module. We set it on the synthesized module's `path` so
/// [`expo_typecheck::check`] (which derives the package from the path
/// stem) lines up with the explicit `__repl__` we hand to
/// [`expo_codegen::lower_modules`]. Without this alignment, types
/// like `Color` end up registered under `__test__` (typecheck's
/// default fallback) but referenced under `__repl__` at lowering
/// time, producing surprising "unknown field" / "0 fields" runtime
/// errors.
const SESSION_PACKAGE: &str = "__repl__";

/// Accumulating REPL session state: every type / fn / impl block the
/// user has typed lives in `item_blocks`, every assignment /
/// expression statement in `statement_blocks`. Each block is the
/// original source text of one user input (multi-line inputs are
/// stored as one block); [`Session::synthesize`] concatenates them
/// into a single Expo module the existing pipeline can typecheck +
/// lower + run.
///
/// Re-execution model: each new input runs the *whole* session, not
/// just the new bit. State persists naturally because everything
/// re-runs. Trades performance (O(history) per input) for simplicity.
pub struct Session {
    /// Bumped per evaluated input. The REPL loop uses it for the
    /// prompt counter; no other consumer today.
    counter: u32,
    /// Source text of every item-shape input (struct / enum / fn /
    /// impl / etc.). Concatenated at module top level when
    /// [`Self::synthesize`] builds the session module.
    item_blocks: Vec<String>,
    /// Source text of every statement-shape input (assignment /
    /// expression / `let` / etc.). Concatenated inside the
    /// synthesized `__expo_session_main__` function body.
    statement_blocks: Vec<String>,
}

impl Session {
    /// Fresh empty session. Counter starts at 1 to match the prompt
    /// numbering users see.
    pub fn new() -> Self {
        Self {
            counter: 1,
            item_blocks: Vec::new(),
            statement_blocks: Vec::new(),
        }
    }

    /// Bump the per-input counter; called by the REPL loop after a
    /// successful evaluation so the next prompt shows N+1.
    pub fn bump_counter(&mut self) {
        self.counter += 1;
    }

    /// Reset to the empty state -- triggered by `:reset`.
    pub fn clear(&mut self) {
        self.counter = 1;
        self.item_blocks.clear();
        self.statement_blocks.clear();
    }

    /// Current per-input counter -- the `N` in `expo(N)>`.
    pub fn counter(&self) -> u32 {
        self.counter
    }

    /// Number of accumulated item-shape blocks; used by the `:state`
    /// command to surface session size.
    pub fn item_count(&self) -> usize {
        self.item_blocks.len()
    }

    /// Number of accumulated statement-shape blocks; used by the
    /// `:state` command to surface session size.
    pub fn statement_count(&self) -> usize {
        self.statement_blocks.len()
    }

    /// Evaluate one user input against this session, mutating it on
    /// success (the input's items / statements get appended) and
    /// rolling back on failure (the session is left exactly as it
    /// was before the call). Returns `Ok(Some(rendered))` when the
    /// input contributed a trailing expression -- the value to
    /// print -- and `Ok(None)` when the input was purely
    /// declarative.
    pub fn try_eval(&mut self, input: &str) -> Result<Option<String>, String> {
        let snapshot = self.snapshot();
        match self.eval_into(input) {
            Ok(rendered) => Ok(rendered),
            Err(error) => {
                self.restore(snapshot);
                Err(error)
            }
        }
    }

    fn eval_into(&mut self, input: &str) -> Result<Option<String>, String> {
        let shape = classify_input(input)?;
        let tail_is_expr = match &shape {
            InputShape::Items(text) => {
                self.item_blocks.push(text.clone());
                false
            }
            InputShape::Statements {
                source,
                tail_is_expr,
            } => {
                self.statement_blocks.push(source.clone());
                *tail_is_expr
            }
        };
        let return_type = if tail_is_expr {
            self.infer_tail_type()?
        } else {
            None
        };
        let return_type_text = return_type
            .as_ref()
            .map(render_type_for_annotation)
            .transpose()?;
        let value = self.run(return_type_text.as_deref())?;
        if !tail_is_expr {
            return Ok(None);
        }
        let ty = return_type.unwrap_or(Type::Unit);
        Ok(Some(format_value_with_type(&value, &ty)))
    }

    /// Probe-typecheck the synthesized session module *without* a
    /// return annotation, then read the resolved type of the
    /// trailing expression inside `__expo_session_main__`. Returns
    /// `Ok(None)` when the trailing expression has no resolved type
    /// -- the caller treats that as "no print value" rather than as
    /// an error so the user still sees any side effects.
    ///
    /// Surfaces typecheck diagnostics (missing fields, unknown names,
    /// type mismatches, ...) as `Err` so the REPL stops before
    /// lowering / interpretation -- otherwise the runtime sees a
    /// half-validated module and reports it as an opaque interpreter
    /// crash.
    fn infer_tail_type(&self) -> Result<Option<Type>, String> {
        let probe_source = self.synthesize(None);
        let mut probe_module = parse_module(&probe_source)?;
        probe_module.path = Some(session_module_path());
        let probe_ctx = expo_typecheck::check(&mut probe_module);
        if !probe_ctx.diagnostics.is_empty() {
            return Err(format_diagnostics(&probe_ctx.diagnostics));
        }
        Ok(session_tail_type(&probe_module))
    }

    /// Synthesize the full session source with the resolved
    /// return-type annotation (or none for declaration-only
    /// sessions), typecheck, lower, and run
    /// `__expo_session_main__` through the interpreter.
    fn run(&self, return_type_text: Option<&str>) -> Result<Value, String> {
        let source = self.synthesize(return_type_text);
        let mut module = parse_module(&source)?;
        module.path = Some(session_module_path());
        let type_ctx = expo_typecheck::check(&mut module);
        if !type_ctx.diagnostics.is_empty() {
            return Err(format_diagnostics(&type_ctx.diagnostics));
        }
        let modules = vec![&module];
        let packages = vec![SESSION_PACKAGE];
        let program =
            expo_codegen::lower_modules(&modules, &packages, &type_ctx, SESSION_PACKAGE, None)
                .map_err(|diagnostics| format_diagnostics(&diagnostics))?;
        let mut interp = Interp::new(Arc::new(program), Arc::new(type_ctx))
            .map_err(|error| error.to_string())?;
        interp
            .call(
                &FunctionIdentifier::new(SYNTHETIC_SESSION_ENTRY),
                Vec::new(),
            )
            .map_err(|error| error.to_string())
    }

    /// Build the full session source as a single Expo module. Items
    /// concatenate at top level; statements concatenate inside a
    /// synthesized `__expo_session_main__` function body. When
    /// `return_type` is `Some`, the function is annotated so the IR
    /// carries the matching return type and the interpreter
    /// propagates the value back to the REPL for printing. When
    /// `None`, we omit the annotation entirely -- the typecheck
    /// default resolves to `Type::Unit` and the lowerer's
    /// fallthrough emits a clean `Return None`.
    fn synthesize(&self, return_type: Option<&str>) -> String {
        let mut buffer = String::new();
        for block in &self.item_blocks {
            buffer.push_str(block);
            if !block.ends_with('\n') {
                buffer.push('\n');
            }
            buffer.push('\n');
        }
        match return_type {
            Some(annotation) => {
                buffer.push_str(&format!("fn {SYNTHETIC_SESSION_ENTRY} -> {annotation}\n"))
            }
            None => buffer.push_str(&format!("fn {SYNTHETIC_SESSION_ENTRY}\n")),
        }
        for block in &self.statement_blocks {
            for line in block.lines() {
                buffer.push_str("  ");
                buffer.push_str(line);
                buffer.push('\n');
            }
        }
        buffer.push_str("end\n");
        buffer
    }

    /// Cheap snapshot of session lengths + counter consumed by
    /// [`Self::restore`] so [`Self::try_eval`] can roll back failed
    /// appends.
    fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            counter: self.counter,
            item_count: self.item_blocks.len(),
            statement_count: self.statement_blocks.len(),
        }
    }

    fn restore(&mut self, snapshot: SessionSnapshot) {
        self.counter = snapshot.counter;
        self.item_blocks.truncate(snapshot.item_count);
        self.statement_blocks.truncate(snapshot.statement_count);
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
struct SessionSnapshot {
    counter: u32,
    item_count: usize,
    statement_count: usize,
}

/// Classification of a single REPL input. Items go to the session's
/// item blocks; statements go to its statement blocks.
enum InputShape {
    /// Top-level declarations (`struct`, `enum`, `fn`, `impl`,
    /// `protocol`, `const`, `alias`, `type`). Carries the original
    /// source text; [`Session::synthesize`] concatenates it at
    /// module top level.
    Items(String),
    /// Statement-shape input (assignment / expression / `let` /
    /// `return`). Carries the original source text;
    /// [`Session::synthesize`] concatenates it inside the synthesized
    /// function body. `tail_is_expr` is `true` when the last
    /// statement in the input is an [`Statement::Expr`] -- drives
    /// the print rule.
    Statements { source: String, tail_is_expr: bool },
}

/// Classify a freshly-typed user input as either item-shape (top-level
/// declarations) or statement-shape (assignments / expressions /
/// returns), preserving the original source text. Tries the
/// item-shape parse first; if it fails or yields no items, the input
/// is wrapped as a function body and re-parsed to get statements.
/// Genuine parse errors surface from the wrapped parse.
fn classify_input(input: &str) -> Result<InputShape, String> {
    let raw = expo_parser::parse(input);
    if raw.errors.is_empty() && !raw.module.items.is_empty() {
        return Ok(InputShape::Items(input.to_string()));
    }
    let wrapped = format!("fn __probe\n  {input}\nend\n");
    let parsed = parse_module(&wrapped)?;
    let tail_is_expr = wrapped_function_tail_is_expr(&parsed, "__probe");
    Ok(InputShape::Statements {
        source: input.to_string(),
        tail_is_expr,
    })
}

/// True when the named function's body ends in [`Statement::Expr`].
/// Drives the REPL's print rule: only inputs whose last statement is
/// a value-producing expression show a result line.
fn wrapped_function_tail_is_expr(module: &Module, name: &str) -> bool {
    for item in &module.items {
        let Item::Function(function) = item else {
            continue;
        };
        if function.name != name {
            continue;
        }
        return function
            .body
            .as_ref()
            .and_then(|body| body.last())
            .map(|stmt| matches!(stmt, Statement::Expr(_)))
            .unwrap_or(false);
    }
    false
}

/// Build the synthetic `Path` used for [`SESSION_PACKAGE`] alignment.
/// The typecheck pipeline calls `path.file_stem()` to derive the
/// package name, so the path's stem must match `SESSION_PACKAGE`
/// exactly. The directory and `.expo` extension are cosmetic; only
/// the stem matters.
fn session_module_path() -> PathBuf {
    PathBuf::from(format!("{SESSION_PACKAGE}.expo"))
}

fn session_tail_type(module: &Module) -> Option<Type> {
    for item in &module.items {
        let Item::Function(function) = item else {
            continue;
        };
        if function.name != SYNTHETIC_SESSION_ENTRY {
            continue;
        }
        let body = function.body.as_ref()?;
        let last = body.last()?;
        let Statement::Expr(expr) = last else {
            return Some(Type::Unit);
        };
        return expr.resolved_type.clone();
    }
    None
}

/// Render an Expo [`Type`] as the source-level annotation text the
/// session's synthesized function uses. Handles primitives, `Unit`,
/// and `Type::Named` (including generic instantiations like
/// `Pair<Int, Int>`); falls back to an error for shapes the parser
/// can't round-trip cleanly (unions, function types, etc.). Callers
/// that hit the error treat the input as "value not printable".
fn render_type_for_annotation(ty: &Type) -> Result<String, String> {
    match ty {
        Type::Named {
            identifier,
            type_args,
        } => {
            let base = identifier.name.clone();
            if type_args.is_empty() {
                return Ok(base);
            }
            let args = type_args
                .iter()
                .map(render_type_for_annotation)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("{base}<{}>", args.join(", ")))
        }
        Type::Primitive(primitive) => Ok(primitive.display().to_string()),
        Type::Unit => Ok("Unit".to_string()),
        other => Err(format!(
            "REPL cannot annotate result type {other:?} for printing"
        )),
    }
}

fn format_value_with_type(value: &Value, ty: &Type) -> String {
    if matches!(ty, Type::Unit) {
        return "()".to_string();
    }
    format!("{value} : {}", short_type(ty))
}

fn short_type(ty: &Type) -> String {
    match ty {
        Type::Named {
            identifier,
            type_args,
        } => {
            let base = identifier.name.clone();
            if type_args.is_empty() {
                return base;
            }
            let args = type_args.iter().map(short_type).collect::<Vec<_>>();
            format!("{base}<{}>", args.join(", "))
        }
        Type::Primitive(primitive) => primitive.display().to_string(),
        Type::Unit => "Unit".to_string(),
        other => format!("{other:?}"),
    }
}
