//! Runtime coverage for the union slice in
//! [`koja_ir_eval::Interpreter`]: a member-typed value flowing
//! through a union slot materializes a [`Value::Union`] carrying the
//! union's mangled symbol, the canonical-position tag, and the boxed
//! payload. A typed-binding `match` arm extracts the payload back out
//! and binds it as the named local for the body's expression.
//!
//! The fixtures pin runtime behavior across the four shapes the
//! goldens exercise:
//!
//! - widening a single member into a 2-/3-member union and matching
//!   the catch-all
//! - alias-fronted unions (`type X = A | B`) — the runtime sees the
//!   underlying tagged-union shape with no alias-specific encoding
//! - enum-in-union: each member is itself a tagged enum, and the
//!   typed-binding arm extracts an `Enum` value the body can match
//!   on again
//! - struct-field union: a struct field holds a union value;
//!   reading the field yields the same `Value::Union` shape
//!   construction would.

use koja_ast::util::dedent;
use koja_ir_eval::Value;

mod common;

fn evaluate_script(source: &str) -> Value {
    common::evaluate_script(source).expect("interpreter should not error on this fixture")
}

#[test]
fn member_widens_into_union_and_match_catch_all_returns() {
    let source = "
        struct Post
          title: String
        end

        struct Comment
          body: String
        end

        fn describe(item: Post | Comment) -> String
          match item
            _ -> \"ok\"
          end
        end

        describe(Post{title: \"hi\"})
        ";
    let value = evaluate_script(&dedent(source));
    let Value::String(bytes) = value else {
        panic!("expected Value::String, got {value:?}");
    };
    assert_eq!(std::str::from_utf8(&bytes).unwrap(), "ok");
}

#[test]
fn typed_binding_arm_extracts_member_payload() {
    // Each typed-binding arm narrows to its member type; the body
    // sees the field projection of the bound payload.
    let source = "
        struct Post
          title: String
        end

        struct Comment
          body: String
        end

        struct Ad
          url: String
        end

        fn describe(item: Post | Comment | Ad) -> String
          match item
            p: Post -> p.title
            c: Comment -> c.body
            a: Ad -> a.url
          end
        end

        describe(Comment{body: \"bee\"})
        ";
    let value = evaluate_script(&dedent(source));
    let Value::String(bytes) = value else {
        panic!("expected Value::String, got {value:?}");
    };
    assert_eq!(std::str::from_utf8(&bytes).unwrap(), "bee");
}

#[test]
fn alias_fronted_union_is_runtime_equivalent_to_bare_union() {
    // The alias is a typecheck convenience — at runtime, both
    // `Pet` and `Cat | Dog | Fish` materialize the same tagged-
    // union shape and match the same way.
    let source = "
        struct Cat
          name: String
        end

        struct Dog
          name: String
        end

        struct Fish
          color: String
        end

        type Pet = Cat | Dog | Fish

        fn describe(p: Pet) -> String
          match p
            c: Cat -> c.name
            d: Dog -> d.name
            f: Fish -> f.color
          end
        end

        describe(Dog{name: \"rex\"})
        ";
    let value = evaluate_script(&dedent(source));
    let Value::String(bytes) = value else {
        panic!("expected Value::String, got {value:?}");
    };
    assert_eq!(std::str::from_utf8(&bytes).unwrap(), "rex");
}

#[test]
fn enum_in_union_runs_nested_match() {
    // A typed-binding arm narrows the union to one of its enum
    // members; the body's inner `match` then dispatches on that
    // enum's variants. Mirrors the `process_union_msg` golden's
    // outer/inner shape.
    let source = "
        enum Sig
          Stop
        end

        enum Tick
          Beat(Int)
        end

        fn handle(msg: Tick | Sig) -> String
          match msg
            t: Tick ->
              match t
                Tick.Beat(n) -> \"beat\"
              end
            s: Sig ->
              match s
                Sig.Stop -> \"stop\"
              end
          end
        end

        handle(Sig.Stop)
        ";
    let value = evaluate_script(&dedent(source));
    let Value::String(bytes) = value else {
        panic!("expected Value::String, got {value:?}");
    };
    assert_eq!(std::str::from_utf8(&bytes).unwrap(), "stop");
}

#[test]
fn struct_field_union_round_trips_through_field_read() {
    // The struct stores a union-typed field; reading the field
    // surfaces the same `Value::Union` shape construction would,
    // so the typed-binding match dispatches identically.
    let source = "
        struct Cat
          name: String
        end

        struct Dog
          name: String
        end

        struct Holder
          pet: Cat | Dog
        end

        fn name_of(h: Holder) -> String
          match h.pet
            c: Cat -> c.name
            d: Dog -> d.name
          end
        end

        name_of(Holder{pet: Cat{name: \"whiskers\"}})
        ";
    let value = evaluate_script(&dedent(source));
    let Value::String(bytes) = value else {
        panic!("expected Value::String, got {value:?}");
    };
    assert_eq!(std::str::from_utf8(&bytes).unwrap(), "whiskers");
}
