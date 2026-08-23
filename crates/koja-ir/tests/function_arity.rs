use std::collections::BTreeSet;

use koja_ir::IRInstruction;

mod common;

use common::{all_instructions, lower_script_source, script_function_names};

#[test]
fn overloads_emit_distinct_symbols_and_call_targets() {
    let script = lower_script_source(
        "
        fn choose(value: Int) -> Int
          value
        end

        fn choose(left: Int, right: Int) -> Int
          left + right
        end

        choose(1)
        choose(2, 3)
        ",
    );

    let names = script_function_names(&script);
    assert!(names.contains(&"TestApp.choose/1".to_string()));
    assert!(names.contains(&"TestApp.choose/2".to_string()));

    let callees: BTreeSet<_> = all_instructions(&script.blocks)
        .filter_map(|instruction| match instruction {
            IRInstruction::Call { callee, .. } => Some(callee.mangled()),
            _ => None,
        })
        .collect();
    assert!(callees.contains("TestApp.choose/1"));
    assert!(callees.contains("TestApp.choose/2"));
}

#[test]
fn method_overloads_keep_self_in_arity() {
    let script = lower_script_source(
        "
        struct Counter
          value: Int

          fn add(self, value: Int) -> Int
            self.value + value
          end

          fn add(self, left: Int, right: Int) -> Int
            self.value + left + right
          end
        end

        counter = Counter{value: 1}
        counter.add(2)
        counter.add(3, 4)
        ",
    );

    let names = script_function_names(&script);
    assert!(names.contains(&"TestApp.Counter.add/2".to_string()));
    assert!(names.contains(&"TestApp.Counter.add/3".to_string()));
}

#[test]
fn default_adapter_and_named_reference_use_selected_arity() {
    let script = lower_script_source(
        "
        fn add(left: Int, right: Int = 4) -> Int
          left + right
        end

        apply = &add/1
        apply(3)
        ",
    );

    let names = script_function_names(&script);
    assert!(names.contains(&"TestApp.add/1".to_string()));
    assert!(names.contains(&"TestApp.add/2".to_string()));
    assert!(names.iter().any(|name| name == "TestApp.add/1__as_closure"));
}

#[test]
fn generic_overloads_monomorphize_without_colliding() {
    let script = lower_script_source(
        "
        fn identity<T>(value: T) -> T
          value
        end

        fn identity<T>(first: T, second: T) -> T
          second
        end

        identity(1)
        identity(2, 3)
        ",
    );

    let names = script_function_names(&script);
    assert!(names.contains(&"TestApp.identity/1_$Int64$".to_string()));
    assert!(names.contains(&"TestApp.identity/2_$Int64$".to_string()));
}
