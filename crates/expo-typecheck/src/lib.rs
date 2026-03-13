mod check;
mod collect;
pub mod context;
pub mod types;

use context::TypeContext;
use expo_ast::ast::Module;

pub fn check(module: &Module) -> TypeContext {
    let mut ctx = collect::collect(module);
    check::check_module(module, &mut ctx);
    ctx
}
