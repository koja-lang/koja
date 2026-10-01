//! Seal checks for the constant pool, shared by the program and script
//! shapes. Every `LoadConst` must name a pool entry, each
//! [`IRConstantValue::Built`] init must be a registered zero-parameter
//! function that returns the constant's type, and the stored startup
//! order must list every `Built` constant once, after every constant
//! its init reaches.

use std::collections::BTreeMap;

use crate::built_order::InitGraph;
use crate::constant::IRConstantValue;
use crate::declarations::Declarations;
use crate::function::{FunctionKind, IRInstruction, IRSymbol};

use super::seal_panic;

/// Every [`IRInstruction::LoadConst`] must name a pool entry the
/// lookup can find. Lower mints both the pool entry and the
/// `LoadConst` referencing it from the same registry-stamped
/// constant, so a miss here indicates a lowering / merge bug.
pub(super) fn seal_loadconst_pool<'inst, 'value>(
    instructions: impl IntoIterator<Item = (String, &'inst IRInstruction)>,
    lookup: &impl Fn(&str) -> Option<&'value IRConstantValue>,
) {
    for (owner, inst) in instructions {
        if let IRInstruction::LoadConst { const_id, .. } = inst
            && lookup(const_id.mangled()).is_none()
        {
            seal_panic(&format!(
                "{owner} loads constant `{const_id}`, but no package has a pool entry for \
                 that symbol",
            ));
        }
    }
}

/// Assert the `Built` invariants over every constant in
/// `declarations` and the `built_constant_order` the lowering entry
/// stored next to them.
pub(super) fn seal_built_constants(declarations: &Declarations<'_>, order: &[IRSymbol]) {
    let mut built: BTreeMap<&IRSymbol, &IRSymbol> = BTreeMap::new();
    for (symbol, value) in declarations.constants() {
        let IRConstantValue::Built { init, ty } = value else {
            continue;
        };
        let Some(function) = declarations.function(init) else {
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
    let graph = InitGraph::new(declarations);
    for (symbol, init) in &built {
        let Some(own) = position.get(symbol) else {
            seal_panic(&format!(
                "built constant `{symbol}` is missing from the built constant order",
            ));
        };
        for reached in graph.reached_constants(init) {
            let before = position
                .get(reached)
                .expect("every built constant has a position by now");
            if before >= own {
                seal_panic(&format!(
                    "built constant `{symbol}` runs before `{reached}`, which its init reaches",
                ));
            }
        }
    }
}
