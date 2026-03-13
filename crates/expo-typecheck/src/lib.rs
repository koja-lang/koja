mod check;
mod collect;
pub mod context;
pub mod types;

use std::collections::HashMap;

use context::TypeContext;
use expo_ast::ast::Module;

pub fn check(module: &Module) -> TypeContext {
    let mut ctx = collect::collect(module);
    check::check_module(module, &mut ctx);
    ctx
}

pub fn collect_module(module: &Module) -> TypeContext {
    collect::collect(module)
}

pub fn resolve_imports(
    module: &Module,
    ctx: &mut TypeContext,
    module_contexts: &HashMap<String, TypeContext>,
) {
    collect::resolve_imports(module, ctx, module_contexts);
}

pub fn check_module(module: &Module, ctx: &mut TypeContext) {
    check::check_module(module, ctx);
}
