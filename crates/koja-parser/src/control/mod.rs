//! Control-flow expressions.
//!
//! Modules:
//! - `conditional` — `if`, `unless`
//! - `loops` — `for`, `loop`, `while`
//! - `match_arms` — `match`, `cond`, `receive`. These three all
//!   parse arm streams that share the same `pattern when guard -> body`
//!   shape, the same "stuck-progress error recovery" loop, and the
//!   same heuristic for spotting where one arm ends and the next
//!   begins ([`Parser::looks_like_new_arm`]).

pub(crate) mod conditional;
pub(crate) mod loops;
pub(crate) mod match_arms;
