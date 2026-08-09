//! Runtime coverage for default field values: constructions that
//! omit defaulted fields arrive at the interpreter fully populated
//! (the fill happens in typecheck resolve), so these tests prove the
//! synthesized inits evaluate to the declared defaults.

use koja_ast::util::dedent;
use koja_ir_eval::Value;

mod common;

fn evaluate_script(source: &str) -> Value {
    common::evaluate_script(source).expect("interpreter should not error on this fixture")
}

#[test]
fn omitted_struct_fields_evaluate_defaults() {
    let source = "
        struct Config
          host: String = \"localhost\"
          port: Int = 5432
          name: String
        end

        c = Config{name: \"app\"}
        \"#{c.host}:#{c.port}/#{c.name}\"
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::string("localhost:5432/app"));
}

#[test]
fn explicit_init_overrides_default_at_runtime() {
    let source = "
        struct Config
          port: Int = 5432
        end

        Config{port: 9000}.port
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(9000));
}

#[test]
fn generic_empty_list_default_evaluates_per_site() {
    let source = "
        struct Stack<T>
          items: List<T> = []
          top: Option<T> = Option.None
        end

        s: Stack<Int> = Stack{}
        t = s.items.append(7)
        t.length() + s.items.length()
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(1));
}

#[test]
fn enum_struct_variant_defaults_evaluate() {
    let source = "
        enum Shape
          Rect{width: Int, height: Int = 2}
        end

        match Shape.Rect{width: 4}
          Shape.Rect{width: w, height: h} -> w * h
        end
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(8));
}

#[test]
fn cross_package_default_evaluates_declaring_package_variant() {
    let dep = "
        enum Mode
          Fast
          Safe
        end

        struct Job
          mode: Mode = Mode.Safe
          retries: Int = 3
        end
        ";
    let script = "
        j = Dep.Job{}
        match j.mode
          Dep.Mode.Fast -> 0
          Dep.Mode.Safe -> j.retries
        end
        ";

    let value = common::evaluate_script_with_dep("Dep", &dedent(dep), &dedent(script))
        .expect("cross-package defaulted construction should evaluate");
    assert_eq!(value, Value::Int(3));
}
