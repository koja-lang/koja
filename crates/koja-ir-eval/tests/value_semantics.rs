//! Value-semantics coverage. Under the new model a binding is an
//! independent value, so assigning a collection to a second binding
//! and mutating one side is never observable through the other.
//! Mutators are copy-on-write (they clone the receiver's backing
//! store before mutating), so `b = a; b = b.append(x)` leaves `a`
//! untouched, including across a function-call boundary, where the
//! callee's local mutation can't reach back to the caller's binding.

use koja_ast::util::dedent;
use koja_ir_eval::Value;

mod common;

use common::evaluate_script as evaluate;

#[test]
fn list_assignment_is_a_copy_not_an_alias() {
    let source = "
        a = [1, 2]
        b = a
        b = b.append(3)
        a.length()
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(2));
}

#[test]
fn list_is_unchanged_after_a_helper_mutates_its_own_binding() {
    let source = "
        fn grow(xs: List<Int>) -> List<Int>
          xs.append(99)
        end

        a = [1, 2]
        ignored = grow(a)
        a.length()
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(2));
}

// The tests below run through the consuming twins the consume-fusion
// pass substitutes when the receiver value is dead at the call site
// (see `koja_ir::elaborate::consume` and `intrinsics::consuming`).
// Value semantics must hold unchanged whether the twin mutates in
// place (uniquely held storage) or falls back to the copying
// original (storage still shared with a slot or an alias).

#[test]
fn fused_rebind_loop_builds_the_full_list() {
    let source = "
        xs: List<Int> = []
        i = 0
        while i < 100
          xs = xs.append(i)
          i += 1
        end
        xs.length()
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(100));
}

#[test]
fn alias_taken_before_a_fused_rebind_keeps_its_value() {
    let source = "
        a = [1, 2]
        b = a
        a = a.append(3)
        a.length() * 10 + b.length()
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(32));
}

#[test]
fn owned_temp_chain_consumes_the_intermediate() {
    let source = "
        seed: List<Int> = List.new()
        seed.append(1).append(2).length()
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(2));
}

#[test]
fn fused_map_and_set_rebinds_preserve_aliases() {
    let source = "
        m: Map<Int, Int> = Map.new()
        m = m.put(1, 10)
        m2 = m
        m = m.put(2, 20)
        s: Set<Int> = Set.new()
        s = s.insert(1)
        s2 = s
        s = s.insert(2)
        m.length() * 1000 + m2.length() * 100 + s.length() * 10 + s2.length()
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(2121));
}
