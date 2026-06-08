//! Value-semantics coverage. Under the new model a binding is an
//! independent value, so assigning a collection to a second binding
//! and mutating one side is never observable through the other.
//! Mutators are copy-on-write (they clone the receiver's backing
//! store before mutating), so `b = a; b = b.append(x)` leaves `a`
//! untouched — including across a function-call boundary, where the
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
