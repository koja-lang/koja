mod check;
mod collect;
pub mod context;
pub mod types;

use std::collections::HashMap;

use context::TypeContext;
use expo_ast::ast::Module;

/// Runs collection and type-checking in one step, returning a populated context.
pub fn check(module: &Module) -> TypeContext {
    let mut ctx = collect::collect(module);
    check::check_module(module, &mut ctx);
    ctx
}

/// Validates all function bodies, expressions, and patterns against the context.
pub fn check_module(module: &Module, ctx: &mut TypeContext) {
    check::check_module(module, ctx);
}

/// Walks the AST to collect type signatures for functions, structs, and enums.
pub fn collect_module(module: &Module) -> TypeContext {
    collect::collect(module)
}

/// Merges imported module contexts into the current context based on import statements.
pub fn resolve_imports(
    module: &Module,
    ctx: &mut TypeContext,
    module_contexts: &HashMap<String, TypeContext>,
) {
    collect::resolve_imports(module, ctx, module_contexts);
}
