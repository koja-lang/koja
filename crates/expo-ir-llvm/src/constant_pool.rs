//! Flat snapshot of every package's pooled compound constants —
//! flattened once at compile entry so [`EmitContext`] can resolve a
//! [`expo_alpha_ir::IRInstruction::LoadConst`] without threading
//! `&[IRPackage]` through [`crate::emit::emit_instruction`]. Keys use
//! [`IRSymbol`] identity (opaque to LLVM), not detached `String`/`&str`,
//! matching [`expo_alpha_ir::IRPackage::constants`].

use std::collections::BTreeMap;
use std::sync::Arc;

use expo_alpha_ir::{IRConstantValue, IRPackage, IRSymbol};

#[derive(Debug)]
pub(crate) struct ConstantPoolSnapshot {
    entries: BTreeMap<IRSymbol, IRConstantValue>,
}

impl ConstantPoolSnapshot {
    pub(crate) fn from_packages(packages: &[IRPackage]) -> Arc<Self> {
        let mut entries = BTreeMap::new();
        for pkg in packages {
            for (sym, val) in &pkg.constants {
                if entries.insert(sym.clone(), val.clone()).is_some() {
                    panic!(
                        "alpha LLVM: duplicate constant pool key `{}` while merging packages",
                        sym,
                    );
                }
            }
        }
        Arc::new(Self { entries })
    }

    pub(crate) fn get(&self, id: &IRSymbol) -> Option<&IRConstantValue> {
        self.entries.get(id)
    }
}
