//! Closure-shaped seal invariants.
//!
//! Two complementary checks:
//!
//! - [`seal_closure_decls`] runs per package and validates every
//!   [`IRFunction`] with [`FunctionKind::Closure`] in isolation —
//!   each `env_layout` slot is a supported [`IRType`], every
//!   `LoadCapture` inside the body has `capture_index < env_layout.len()`
//!   and `ty == env_layout[capture_index]`. It also guards against
//!   stray `LoadCapture` in non-closure functions.
//! - [`seal_closure_ops`] runs across the assembled
//!   [`crate::IRProgram`] / [`crate::IRScript`] (call site supplies
//!   the cross-package function lookup) and validates every
//!   [`IRInstruction::MakeClosure`] against its body decl: the body
//!   symbol resolves, the resolved function is `FunctionKind::Closure`,
//!   the `captures` arity matches `env_layout.len()`, and the
//!   `IRType::Function` value type matches the body's
//!   `params`/`return_type` signature.
//!
//! Both checks panic on violation through [`super::seal_panic`] —
//! closure seal failures indicate a [`crate::lower::closures`] bug,
//! not a user error.

use crate::function::{FunctionKind, IRFunction, IRInstruction, IRSymbol};
use crate::package::IRPackage;
use crate::types::IRType;

use super::{require_supported_type, seal_panic};

pub(super) fn seal_closure_decls(pkg: &IRPackage) {
    for function in pkg.functions.values() {
        match &function.kind {
            FunctionKind::Closure { env_layout } => seal_closure_function(function, env_layout),
            FunctionKind::Extern(_) | FunctionKind::Intrinsic(_) | FunctionKind::Regular => {
                forbid_loadcapture_in(function);
            }
        }
    }
}

/// Per-closure body invariants: every `env_layout` slot is in the
/// supported set, and every `LoadCapture` keyed into that layout
/// is in range with a matching `ty`. Also enforces unique capture
/// reads at the layout level (a layout slot may be read many
/// times, that's fine — what's not fine is an out-of-range index).
fn seal_closure_function(function: &IRFunction, env_layout: &[IRType]) {
    let owner = format!("closure `{}`", function.symbol);
    for (index, ty) in env_layout.iter().enumerate() {
        require_supported_type(ty, &|| format!("{owner} env_layout[{index}]"));
    }
    for block in &function.blocks {
        for inst in &block.instructions {
            if let IRInstruction::LoadCapture {
                capture_index, ty, ..
            } = inst
            {
                let Some(declared) = env_layout.get(*capture_index as usize) else {
                    seal_panic(&format!(
                        "{owner} block {} reads capture #{capture_index}, but env_layout has \
                         only {count} slot(s)",
                        block.id,
                        count = env_layout.len(),
                    ));
                };
                if declared != ty {
                    seal_panic(&format!(
                        "{owner} block {}: LoadCapture #{capture_index} carries ty `{got:?}` \
                         but env_layout[{capture_index}] is `{expected:?}`",
                        block.id,
                        got = ty,
                        expected = declared,
                    ));
                }
            }
        }
    }
}

/// `LoadCapture` is only well-defined inside a closure body.
/// `Regular` / `Intrinsic` / `Extern` functions have no env slot,
/// so a stray `LoadCapture` here is always a lowering bug.
fn forbid_loadcapture_in(function: &IRFunction) {
    let owner = format!("function `{}`", function.symbol);
    for block in &function.blocks {
        for inst in &block.instructions {
            if let IRInstruction::LoadCapture { capture_index, .. } = inst {
                seal_panic(&format!(
                    "{owner} block {} emits LoadCapture #{capture_index} but the function is \
                     not a closure body — only `FunctionKind::Closure` admits captures",
                    block.id,
                ));
            }
        }
    }
}

/// Cross-instruction closure check. Driven by the `(owner, inst)`
/// stream the caller produces (see
/// [`super::structs::package_instructions`] /
/// [`super::structs::script_body_instructions`]); `lookup`
/// resolves an [`IRSymbol::mangled`] view to the registered
/// [`IRFunction`] (`IRProgram::function` / `IRScript::function`).
pub(super) fn seal_closure_ops<'inst, 'fun>(
    instructions: impl IntoIterator<Item = (String, &'inst IRInstruction)>,
    lookup: &impl Fn(&str) -> Option<&'fun IRFunction>,
) {
    for (owner, inst) in instructions {
        if let IRInstruction::MakeClosure {
            body, captures, ty, ..
        } = inst
        {
            let function = require_function(lookup, body, &owner);
            let env_layout = require_closure_kind(function, &owner);
            if captures.len() != env_layout.len() {
                seal_panic(&format!(
                    "{owner}: MakeClosure for `{body}` carries {got} capture(s) but the body's \
                     env_layout has {expected} slot(s)",
                    got = captures.len(),
                    expected = env_layout.len(),
                ));
            }
            require_value_type_matches_body(&owner, body, function, ty);
        }
    }
}

fn require_function<'fun>(
    lookup: &impl Fn(&str) -> Option<&'fun IRFunction>,
    symbol: &IRSymbol,
    owner: &str,
) -> &'fun IRFunction {
    lookup(symbol.mangled()).unwrap_or_else(|| {
        seal_panic(&format!(
            "{owner}: MakeClosure body `{symbol}` is not registered in any package",
        ))
    })
}

fn require_closure_kind<'fun>(function: &'fun IRFunction, owner: &str) -> &'fun [IRType] {
    match &function.kind {
        FunctionKind::Closure { env_layout } => env_layout,
        other => seal_panic(&format!(
            "{owner}: MakeClosure body `{}` is `{kind}` — only `FunctionKind::Closure` is \
             admitted as a closure body",
            function.symbol,
            kind = kind_label(other),
        )),
    }
}

/// Validate that the closure value's [`IRType::Function`] signature
/// matches the synthesized body's exposed (`env_ptr`-stripped)
/// signature. Body `params` carry the user-visible arity in IR
/// (the implicit `env_ptr` is encoded by `FunctionKind::Closure`,
/// not in the param list), so the match is positional one-for-one.
fn require_value_type_matches_body(
    owner: &str,
    body: &IRSymbol,
    function: &IRFunction,
    value_ty: &IRType,
) {
    let IRType::Function {
        params: ty_params,
        ret: ty_ret,
    } = value_ty
    else {
        seal_panic(&format!(
            "{owner}: MakeClosure for `{body}` carries ty `{value_ty:?}` — closure values must \
             have type `IRType::Function`",
        ));
    };
    if ty_params.len() != function.params.len() {
        seal_panic(&format!(
            "{owner}: MakeClosure for `{body}` value-type has {got} param(s) but the body's \
             user-visible arity is {expected}",
            got = ty_params.len(),
            expected = function.params.len(),
        ));
    }
    for (index, (declared, actual)) in function
        .params
        .iter()
        .map(|p| &p.ty)
        .zip(ty_params.iter())
        .enumerate()
    {
        if declared != actual {
            seal_panic(&format!(
                "{owner}: MakeClosure for `{body}` value-type param[{index}] is `{actual:?}` \
                 but the body declares `{declared:?}`",
            ));
        }
    }
    if function.return_type != **ty_ret {
        seal_panic(&format!(
            "{owner}: MakeClosure for `{body}` value-type ret is `{got:?}` but the body \
             returns `{expected:?}`",
            got = ty_ret,
            expected = function.return_type,
        ));
    }
}

fn kind_label(kind: &FunctionKind) -> &'static str {
    match kind {
        FunctionKind::Closure { .. } => "Closure",
        FunctionKind::Extern(_) => "Extern",
        FunctionKind::Intrinsic(_) => "Intrinsic",
        FunctionKind::Regular => "Regular",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use expo_ast::identifier::{Identifier, LocalId};

    use crate::function::{
        FunctionKind, IRBasicBlock, IRBlockId, IRFunction, IRFunctionParam, IRInstruction,
        IRSymbol, IRTerminator,
    };
    use crate::local::IRLocalId;
    use crate::ownership::Ownership;
    use crate::package::IRPackage;
    use crate::types::{IRType, ValueId};

    use super::{seal_closure_decls, seal_closure_ops};

    fn symbol(name: &str) -> IRSymbol {
        IRSymbol::from_identifier(&Identifier::new("TestApp", vec![name.to_string()]))
    }

    /// Build a minimal `FunctionKind::Closure` body with the given
    /// `env_layout` and a single user-visible `Int64` param. The
    /// entry block returns the param.
    fn closure_function(name: &str, env_layout: Vec<IRType>) -> IRFunction {
        let sym = symbol(name);
        let param_id = ValueId(0);
        let param_local = IRLocalId::from_local_id(LocalId::new(0));
        let entry = IRBasicBlock {
            id: IRBlockId(0),
            label: "entry".to_string(),
            params: Vec::new(),
            instructions: vec![
                IRInstruction::LocalDecl {
                    local: param_local,
                    ty: IRType::Int64,
                },
                IRInstruction::LocalWrite {
                    local: param_local,
                    ownership: Ownership::Unowned,
                    value: param_id,
                },
            ],
            terminator: IRTerminator::Return {
                value: Some(param_id),
            },
        };
        IRFunction {
            blocks: vec![entry],
            kind: FunctionKind::Closure { env_layout },
            params: vec![IRFunctionParam {
                id: param_id,
                local_id: param_local,
                ty: IRType::Int64,
            }],
            return_type: IRType::Int64,
            symbol: sym,
        }
    }

    fn regular_function_with(instructions: Vec<IRInstruction>) -> IRFunction {
        let sym = symbol("Outer");
        let entry = IRBasicBlock {
            id: IRBlockId(0),
            label: "entry".to_string(),
            params: Vec::new(),
            instructions,
            terminator: IRTerminator::Return { value: None },
        };
        IRFunction {
            blocks: vec![entry],
            kind: FunctionKind::Regular,
            params: Vec::new(),
            return_type: IRType::Unit,
            symbol: sym,
        }
    }

    fn package_with(functions: Vec<IRFunction>) -> IRPackage {
        let mut map = BTreeMap::new();
        for function in functions {
            map.insert(function.symbol.clone(), function);
        }
        IRPackage {
            constants: BTreeMap::new(),
            enums: BTreeMap::new(),
            functions: map,
            package: "TestApp".to_string(),
            structs: BTreeMap::new(),
        }
    }

    fn lookup_against<'a>(funs: &'a [IRFunction]) -> impl Fn(&str) -> Option<&'a IRFunction> + 'a {
        move |needle: &str| funs.iter().find(|f| f.symbol.mangled() == needle)
    }

    #[test]
    fn well_formed_closure_decl_passes_seal() {
        let pkg = package_with(vec![closure_function("F__closure0", vec![IRType::Int64])]);
        seal_closure_decls(&pkg);
    }

    #[test]
    #[should_panic(expected = "reads capture #1, but env_layout has only 1 slot")]
    fn out_of_range_load_capture_panics() {
        let mut function = closure_function("F__closure0", vec![IRType::Int64]);
        function.blocks[0].instructions.insert(
            0,
            IRInstruction::LoadCapture {
                capture_index: 1,
                dest: ValueId(99),
                ty: IRType::Int64,
            },
        );
        seal_closure_decls(&package_with(vec![function]));
    }

    #[test]
    #[should_panic(expected = "carries ty `Bool` but env_layout[0] is `Int64`")]
    fn load_capture_type_mismatch_panics() {
        let mut function = closure_function("F__closure0", vec![IRType::Int64]);
        function.blocks[0].instructions.insert(
            0,
            IRInstruction::LoadCapture {
                capture_index: 0,
                dest: ValueId(99),
                ty: IRType::Bool,
            },
        );
        seal_closure_decls(&package_with(vec![function]));
    }

    #[test]
    #[should_panic(expected = "the function is not a closure body")]
    fn stray_load_capture_in_regular_function_panics() {
        let function = regular_function_with(vec![IRInstruction::LoadCapture {
            capture_index: 0,
            dest: ValueId(0),
            ty: IRType::Int64,
        }]);
        seal_closure_decls(&package_with(vec![function]));
    }

    #[test]
    fn well_formed_make_closure_passes_seal() {
        let body = closure_function("F__closure0", vec![IRType::Int64]);
        let make = IRInstruction::MakeClosure {
            body: body.symbol.clone(),
            captures: vec![ValueId(7)],
            dest: ValueId(8),
            ty: IRType::Function {
                params: vec![IRType::Int64],
                ret: Box::new(IRType::Int64),
            },
        };
        let funs = vec![body];
        seal_closure_ops(
            std::iter::once(("test".to_string(), &make)),
            &lookup_against(&funs),
        );
    }

    #[test]
    #[should_panic(expected = "is not registered in any package")]
    fn make_closure_with_unknown_body_panics() {
        let make = IRInstruction::MakeClosure {
            body: symbol("Missing"),
            captures: Vec::new(),
            dest: ValueId(0),
            ty: IRType::Function {
                params: Vec::new(),
                ret: Box::new(IRType::Unit),
            },
        };
        let funs: Vec<IRFunction> = Vec::new();
        seal_closure_ops(
            std::iter::once(("test".to_string(), &make)),
            &lookup_against(&funs),
        );
    }

    #[test]
    #[should_panic(expected = "is `Regular` — only `FunctionKind::Closure`")]
    fn make_closure_pointing_at_regular_panics() {
        let target = regular_function_with(Vec::new());
        let make = IRInstruction::MakeClosure {
            body: target.symbol.clone(),
            captures: Vec::new(),
            dest: ValueId(0),
            ty: IRType::Function {
                params: Vec::new(),
                ret: Box::new(IRType::Unit),
            },
        };
        let funs = vec![target];
        seal_closure_ops(
            std::iter::once(("test".to_string(), &make)),
            &lookup_against(&funs),
        );
    }

    #[test]
    #[should_panic(expected = "carries 0 capture(s) but the body's env_layout has 1 slot")]
    fn make_closure_capture_arity_mismatch_panics() {
        let body = closure_function("F__closure0", vec![IRType::Int64]);
        let make = IRInstruction::MakeClosure {
            body: body.symbol.clone(),
            captures: Vec::new(),
            dest: ValueId(0),
            ty: IRType::Function {
                params: vec![IRType::Int64],
                ret: Box::new(IRType::Int64),
            },
        };
        let funs = vec![body];
        seal_closure_ops(
            std::iter::once(("test".to_string(), &make)),
            &lookup_against(&funs),
        );
    }

    #[test]
    #[should_panic(expected = "value-type param[0] is `Bool` but the body declares `Int64`")]
    fn make_closure_value_type_param_mismatch_panics() {
        let body = closure_function("F__closure0", vec![IRType::Int64]);
        let make = IRInstruction::MakeClosure {
            body: body.symbol.clone(),
            captures: vec![ValueId(0)],
            dest: ValueId(1),
            ty: IRType::Function {
                params: vec![IRType::Bool],
                ret: Box::new(IRType::Int64),
            },
        };
        let funs = vec![body];
        seal_closure_ops(
            std::iter::once(("test".to_string(), &make)),
            &lookup_against(&funs),
        );
    }

    #[test]
    #[should_panic(expected = "value-type ret is `Bool` but the body returns `Int64`")]
    fn make_closure_value_type_ret_mismatch_panics() {
        let body = closure_function("F__closure0", vec![IRType::Int64]);
        let make = IRInstruction::MakeClosure {
            body: body.symbol.clone(),
            captures: vec![ValueId(0)],
            dest: ValueId(1),
            ty: IRType::Function {
                params: vec![IRType::Int64],
                ret: Box::new(IRType::Bool),
            },
        };
        let funs = vec![body];
        seal_closure_ops(
            std::iter::once(("test".to_string(), &make)),
            &lookup_against(&funs),
        );
    }
}
