//! Seal checks for the constant pool, shared by the program and script
//! shapes. Every `LoadConst` must name a pool entry, each
//! [`IRConstantValue::Struct`] must supply one value of the declared
//! type for each field, each [`IRConstantValue::Built`] init must be a
//! registered zero-parameter function that returns the constant's
//! type, and the stored startup order must list every `Built` constant
//! once, after every constant its init reaches.

use std::collections::BTreeMap;

use crate::built_order::InitGraph;
use crate::constant::IRConstantValue;
use crate::declarations::Declarations;
use crate::function::{FunctionKind, IRInstruction, IRSymbol};

use super::types::constant_type;
use super::{field_type_matches, seal_panic};

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

/// Every [`IRConstantValue::Struct`] in the pool, at any depth, must
/// name a registered struct and supply one value of the declared type
/// for each field. Backends place the field values by position and
/// trust their types. A value of the wrong type would otherwise
/// surface as a backend type error or as a wrong value at run time.
pub(super) fn seal_struct_constants(declarations: &Declarations<'_>) {
    for (symbol, value) in declarations.constants() {
        seal_struct_constant_fields(symbol, value, declarations);
    }
}

/// Check `value`, a pool entry of the constant `owner` or a value
/// nested in one, against its struct declaration.
fn seal_struct_constant_fields(
    owner: &IRSymbol,
    value: &IRConstantValue,
    declarations: &Declarations<'_>,
) {
    let IRConstantValue::Struct { fields, ty } = value else {
        return;
    };
    let Some(decl) = declarations.struct_decl(ty) else {
        seal_panic(&format!(
            "constant `{owner}` holds struct `{ty}`, which no package registers",
        ));
    };
    if fields.len() != decl.fields.len() {
        seal_panic(&format!(
            "constant `{owner}` supplies {} field value(s) for struct `{ty}`, which declares {}",
            fields.len(),
            decl.fields.len(),
        ));
    }
    for (field, value) in decl.fields.iter().zip(fields) {
        let actual = constant_type(value);
        if !field_type_matches(&field.ir_type, &actual) {
            seal_panic(&format!(
                "constant `{owner}` field `{}` of struct `{ty}` has type `{actual:?}`, \
                 expected `{:?}`",
                field.name, field.ir_type,
            ));
        }
        seal_struct_constant_fields(owner, value, declarations);
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

// Inline unit coverage for the struct constant invariants. Lowering
// produces a well-formed pool by construction, so the only way to
// drive a violation is to hand-build a malformed pool entry.
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use koja_ast::identifier::Identifier;

    use crate::constant::IRConstantValue;
    use crate::declarations::Declarations;
    use crate::function::IRSymbol;
    use crate::package::IRPackage;
    use crate::struct_decl::{IRStructDecl, IRStructField};
    use crate::types::{ConstValue, IRType};

    use super::seal_struct_constants;

    fn symbol(name: &str) -> IRSymbol {
        IRSymbol::from_identifier(&Identifier::single("TestApp", name))
    }

    fn field(index: u32, name: &str, ir_type: IRType) -> IRStructField {
        IRStructField {
            index,
            ir_type,
            name: name.to_string(),
        }
    }

    fn int(value: i64) -> IRConstantValue {
        IRConstantValue::Primitive(ConstValue::Int64(value))
    }

    fn point(fields: Vec<IRConstantValue>) -> IRConstantValue {
        IRConstantValue::Struct {
            fields,
            ty: symbol("Point"),
        }
    }

    /// A package that declares `Point{x: Int64, y: Int64}`, a `Line`
    /// that holds two points, and the constant `ORIGIN` with `value`.
    fn package_with(value: IRConstantValue) -> IRPackage {
        let point = IRStructDecl {
            fields: vec![field(0, "x", IRType::Int64), field(1, "y", IRType::Int64)],
            symbol: symbol("Point"),
        };
        let line = IRStructDecl {
            fields: vec![
                field(0, "from", IRType::Struct(symbol("Point"))),
                field(1, "to", IRType::Struct(symbol("Point"))),
            ],
            symbol: symbol("Line"),
        };
        IRPackage {
            constants: BTreeMap::from([(symbol("ORIGIN"), value)]),
            enums: BTreeMap::new(),
            functions: BTreeMap::new(),
            package: "TestApp".to_string(),
            structs: BTreeMap::from([(point.symbol.clone(), point), (line.symbol.clone(), line)]),
            unions: BTreeMap::new(),
        }
    }

    fn seal(value: IRConstantValue) {
        let packages = [package_with(value)];
        seal_struct_constants(&Declarations::new(&packages));
    }

    #[test]
    fn well_formed_struct_constants_pass_seal() {
        seal(point(vec![int(0), int(0)]));
        seal(IRConstantValue::Struct {
            fields: vec![point(vec![int(0), int(0)]), point(vec![int(1), int(1)])],
            ty: symbol("Line"),
        });
    }

    #[test]
    #[should_panic(
        expected = "constant `TestApp.ORIGIN` field `y` of struct `TestApp.Point` has type `Int32`, expected `Int64`"
    )]
    fn field_value_of_the_wrong_type_panics() {
        seal(point(vec![
            int(0),
            IRConstantValue::Primitive(ConstValue::Int32(0)),
        ]));
    }

    #[test]
    #[should_panic(
        expected = "supplies 1 field value(s) for struct `TestApp.Point`, which declares 2"
    )]
    fn missing_field_value_panics() {
        seal(point(vec![int(0)]));
    }

    #[test]
    #[should_panic(expected = "field `x` of struct `TestApp.Point` has type `String`")]
    fn nested_field_value_of_the_wrong_type_panics() {
        seal(IRConstantValue::Struct {
            fields: vec![
                point(vec![int(0), int(0)]),
                point(vec![
                    IRConstantValue::Primitive(ConstValue::String("x".to_string())),
                    int(1),
                ]),
            ],
            ty: symbol("Line"),
        });
    }

    #[test]
    #[should_panic(expected = "holds struct `TestApp.Missing`, which no package registers")]
    fn unregistered_struct_panics() {
        seal(IRConstantValue::Struct {
            fields: Vec::new(),
            ty: symbol("Missing"),
        });
    }
}
