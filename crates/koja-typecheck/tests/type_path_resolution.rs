//! Typecheck stamps a `Resolution` on every type path.
//!
//! `TypeExpr::Named` and `TypeExpr::Generic` carry `resolution`, and
//! the enum and struct pattern shapes carry `type_resolution`. Lift
//! writes the signature stamps, resolve writes the body stamps, and
//! consumers read them instead of resolving names again.

use koja_ast::ast::{ExprKind, Item, Param, Pattern, Statement, TypeExpr};
use koja_ast::identifier::{Resolution, TypeParamIndex};
use koja_ast::util::dedent;

mod common;

use common::{
    PACKAGE, find_function, registry_id, script_body, test_file, trailing_expr, typecheck_file,
    typecheck_script, typecheck_script_fail,
};

/// Head stamp of a named or generic type expression.
fn head(ty: &TypeExpr) -> Resolution {
    match ty {
        TypeExpr::Named { resolution, .. } | TypeExpr::Generic { resolution, .. } => *resolution,
        other => panic!("expected a named or generic type, got {other:?}"),
    }
}

fn param_type(param: &Param) -> &TypeExpr {
    match param {
        Param::Regular { type_expr, .. } => type_expr,
        Param::Self_ { .. } => panic!("expected a regular parameter"),
    }
}

#[test]
fn signature_type_paths_carry_stamps() {
    let source = "
        struct Point
          x: Int
        end

        type Alias = Point

        fn wrap<T>(items: List<T>, p: Alias) -> Point
          p
        end
        ";
    let checked = typecheck_file(&dedent(source));
    let int = Resolution::Global(registry_id(&checked, "Global", &["Int"]));
    let list = Resolution::Global(registry_id(&checked, "Global", &["List"]));
    let point = Resolution::Global(registry_id(&checked, PACKAGE, &["Point"]));
    let alias = Resolution::Global(registry_id(&checked, PACKAGE, &["Alias"]));
    let wrap = registry_id(&checked, PACKAGE, &["wrap"]);

    let decl = common::find_struct_decl(&checked, "Point");
    assert_eq!(head(&decl.fields[0].type_expr), int, "struct field type");

    let alias_decl = test_file(&checked)
        .items
        .iter()
        .find_map(|item| match item {
            Item::TypeAlias(alias) => Some(alias),
            _ => None,
        })
        .expect("type alias declared");
    assert_eq!(head(&alias_decl.type_expr), point, "alias target");

    let function = find_function(&checked, "wrap");
    let items = param_type(&function.params[0]);
    assert_eq!(head(items), list, "generic head");
    let TypeExpr::Generic { args, .. } = items else {
        panic!("expected `List<T>`, got {items:?}");
    };
    assert_eq!(
        head(&args[0]),
        Resolution::TypeParam {
            owner: wrap,
            index: TypeParamIndex::new(0),
        },
        "type parameter argument"
    );
    assert_eq!(
        head(param_type(&function.params[1])),
        alias,
        "an alias reference stamps the alias entry, not its target"
    );
    let return_type = function.return_type.as_ref().expect("declared return type");
    assert_eq!(head(return_type), point, "return type");
}

#[test]
fn body_annotations_carry_stamps() {
    let source = "
        struct Cat
          name: String
        end

        struct Dog
          name: String
        end

        type Pet = Cat | Dog

        pet: Pet = Cat{name: \"Whiskers\"}
        f = fn (c: Cat) -> Dog
          Dog{name: c.name}
        end
        ";
    let checked = typecheck_script(&dedent(source));
    let cat = Resolution::Global(registry_id(&checked, PACKAGE, &["Cat"]));
    let dog = Resolution::Global(registry_id(&checked, PACKAGE, &["Dog"]));
    let pet = Resolution::Global(registry_id(&checked, PACKAGE, &["Pet"]));

    let alias_decl = test_file(&checked)
        .items
        .iter()
        .find_map(|item| match item {
            Item::TypeAlias(alias) => Some(alias),
            _ => None,
        })
        .expect("type alias declared");
    let TypeExpr::Union { types, .. } = &alias_decl.type_expr else {
        panic!("expected a union alias, got {:?}", alias_decl.type_expr);
    };
    assert_eq!(head(&types[0]), cat, "first union member");
    assert_eq!(head(&types[1]), dog, "second union member");

    let body = script_body(&checked);
    let Statement::Assignment {
        type_annotation: Some(annotation),
        ..
    } = &body[0]
    else {
        panic!("expected an annotated assignment, got {:?}", body[0]);
    };
    assert_eq!(head(annotation), pet, "assignment annotation");

    let Statement::Assignment { value, .. } = &body[1] else {
        panic!("expected the closure assignment, got {:?}", body[1]);
    };
    let ExprKind::Closure {
        params,
        return_type,
        ..
    } = &value.kind
    else {
        panic!("expected a closure, got {:?}", value.kind);
    };
    let koja_ast::ast::ClosureParam::Name {
        type_expr: Some(param_type),
        ..
    } = &params[0]
    else {
        panic!(
            "expected an annotated closure parameter, got {:?}",
            params[0]
        );
    };
    assert_eq!(head(param_type), cat, "closure parameter annotation");
    let return_type = return_type.as_ref().expect("closure return annotation");
    assert_eq!(head(return_type), dog, "closure return annotation");
}

#[test]
fn pattern_type_paths_carry_stamps() {
    let source = "
        enum Shape
          Circle
          Rect{width: Int, height: Int}
          Square(Int)
        end

        struct Point
          x: Int
        end

        s = Shape.Circle
        p = Point{x: 1}
        o = Option.Some(1)

        a = match s
          Shape.Circle -> 1
          Shape.Rect{width: w, height: h} -> w + h
          Shape.Square(n) -> n
        end

        b = match p
          Point{x: n} -> n
        end

        match o
          Some(v) -> v
          None -> 0
        end
        ";
    let checked = typecheck_script(&dedent(source));
    let shape = Resolution::Global(registry_id(&checked, PACKAGE, &["Shape"]));
    let point = Resolution::Global(registry_id(&checked, PACKAGE, &["Point"]));
    let option = Resolution::Global(registry_id(&checked, "Global", &["Option"]));
    let body = script_body(&checked);

    let Statement::Assignment { value, .. } = &body[3] else {
        panic!("expected the Shape match, got {:?}", body[3]);
    };
    let ExprKind::Match { arms, .. } = &value.kind else {
        panic!("expected a match, got {:?}", value.kind);
    };
    let [unit, rect, square] = arms.as_slice() else {
        panic!("expected three arms, got {arms:?}");
    };
    let Pattern::EnumUnit {
        type_resolution, ..
    } = &unit.pattern
    else {
        panic!("expected an enum unit pattern, got {:?}", unit.pattern);
    };
    assert_eq!(*type_resolution, shape, "unit variant pattern");
    let Pattern::EnumStruct {
        type_resolution, ..
    } = &rect.pattern
    else {
        panic!("expected an enum struct pattern, got {:?}", rect.pattern);
    };
    assert_eq!(*type_resolution, shape, "struct variant pattern");
    let Pattern::EnumTuple {
        type_resolution, ..
    } = &square.pattern
    else {
        panic!("expected an enum tuple pattern, got {:?}", square.pattern);
    };
    assert_eq!(*type_resolution, shape, "tuple variant pattern");

    let Statement::Assignment { value, .. } = &body[4] else {
        panic!("expected the Point match, got {:?}", body[4]);
    };
    let ExprKind::Match { arms, .. } = &value.kind else {
        panic!("expected a match, got {:?}", value.kind);
    };
    let Pattern::Struct {
        type_resolution, ..
    } = &arms[0].pattern
    else {
        panic!("expected a struct pattern, got {:?}", arms[0].pattern);
    };
    assert_eq!(*type_resolution, point, "plain struct pattern");

    let ExprKind::Match { arms, .. } = &trailing_expr(&checked).kind else {
        panic!("expected the Option match");
    };
    let Pattern::EnumTuple {
        type_resolution, ..
    } = &arms[0].pattern
    else {
        panic!(
            "expected `Some(v)` to rewrite to an enum tuple pattern, got {:?}",
            arms[0].pattern
        );
    };
    assert_eq!(
        *type_resolution, option,
        "constructor shorthand after rewrite"
    );
    let Pattern::EnumUnit {
        type_resolution, ..
    } = &arms[1].pattern
    else {
        panic!(
            "expected `None` to rewrite to an enum unit pattern, got {:?}",
            arms[1].pattern
        );
    };
    assert_eq!(
        *type_resolution, option,
        "unit constructor shorthand after rewrite"
    );
}

#[test]
fn unknown_type_path_stays_unresolved_and_failure_keeps_registry() {
    let source = "
        fn f(x: Missing) -> Int
          1
        end
        ";
    let failure = typecheck_script_fail(&dedent(source));
    assert!(
        failure.registry.is_some(),
        "typecheck failures keep the registry for best-effort consumers"
    );
    let file = failure
        .partial
        .files
        .values()
        .find(|file| file.package == PACKAGE)
        .expect("partial program keeps the test file");
    let function = file
        .ast
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function) if function.name == "f" => Some(function),
            _ => None,
        })
        .expect("fn f survives in the partial program");
    assert_eq!(
        head(param_type(&function.params[0])),
        Resolution::Unresolved,
        "unknown type name"
    );
    let return_type = function.return_type.as_ref().expect("declared return type");
    assert!(
        head(return_type).is_resolved(),
        "the known return type is still stamped on the failure path"
    );
}
