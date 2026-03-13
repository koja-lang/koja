mod check;
mod collect;
mod context;
mod types;

use expo_ast::ast::{Diagnostic, Module};

pub fn check(module: &Module) -> Vec<Diagnostic> {
    let mut ctx = collect::collect(module);
    check::check_module(module, &mut ctx);
    ctx.diagnostics
}
