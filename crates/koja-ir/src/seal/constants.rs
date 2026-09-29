//! Seal checks for [`IRConstantValue::Built`] pool entries, shared by
//! the program and script shapes. Each init must be a registered
//! zero-parameter function that returns the constant's type, and the
//! stored startup order must list every `Built` constant once, after
//! every constant its init reaches.

use std::collections::BTreeMap;

use crate::built_order::reached_constants;
use crate::constant::IRConstantValue;
use crate::function::{FunctionKind, IRFunction, IRSymbol};
use crate::package::IRPackage;

use super::seal_panic;

/// Assert the `Built` invariants over `packages` and the
/// `built_constant_order` the lowering entry stored next to them.
/// `lookup` resolves a mangled function name the way the owning
/// program or script does.
pub(super) fn seal_built_constants<'a>(
    packages: &'a [IRPackage],
    order: &[IRSymbol],
    lookup: &dyn Fn(&str) -> Option<&'a IRFunction>,
) {
    let mut built: BTreeMap<&IRSymbol, &IRSymbol> = BTreeMap::new();
    for pkg in packages {
        for (symbol, value) in &pkg.constants {
            let IRConstantValue::Built { init, ty } = value else {
                continue;
            };
            let Some(function) = lookup(init.mangled()) else {
                seal_panic(&format!(
                    "built constant `{symbol}` names init `{init}`, which no package registers",
                ));
            };
            if function.kind != FunctionKind::Regular {
                seal_panic(&format!(
                    "built constant `{symbol}` init `{init}` is not a regular function (got `{:?}`)",
                    function.kind,
                ));
            }
            if !function.params.is_empty() {
                seal_panic(&format!(
                    "built constant `{symbol}` init `{init}` takes {} parameter(s), expected none",
                    function.params.len(),
                ));
            }
            if function.return_type != *ty {
                seal_panic(&format!(
                    "built constant `{symbol}` has type `{ty:?}`, but init `{init}` returns `{:?}`",
                    function.return_type,
                ));
            }
            built.insert(symbol, init);
        }
    }

    let mut position: BTreeMap<&IRSymbol, usize> = BTreeMap::new();
    for (index, symbol) in order.iter().enumerate() {
        if !built.contains_key(symbol) {
            seal_panic(&format!(
                "built constant order lists `{symbol}`, which is not a built constant",
            ));
        }
        if position.insert(symbol, index).is_some() {
            seal_panic(&format!(
                "built constant order lists `{symbol}` more than once"
            ));
        }
    }
    for (symbol, init) in &built {
        let Some(own) = position.get(symbol) else {
            seal_panic(&format!(
                "built constant `{symbol}` is missing from the built constant order",
            ));
        };
        for reached in reached_constants(init, packages) {
            let before = position
                .get(&reached)
                .expect("every built constant has a position by now");
            if before >= own {
                seal_panic(&format!(
                    "built constant `{symbol}` runs before `{reached}`, which its init reaches",
                ));
            }
        }
    }
}
