//! Phase 2 typecheck coverage for concrete-instantiation `impl`
//! blocks (Slice 2.9). Expo's "extend"-style model: an `impl` on a
//! concrete instantiation (`impl Render for Bag<Int>`, `impl Bag<Int> { ... }`)
//! adds methods to the concrete domain only. Specialized and
//! generic impls must not define overlapping method names — that's
//! a hard error.
//!
//! Adjacent coverage:
//! - `structs.rs` carries the call-site dispatch path for matching
//!   and mismatched receivers (`trait_impl_on_concrete_target_args_*`)
//!   and the cross-impl method-name collision
//!   (`general_and_specialized_trait_impls_collide_on_shared_method_name`).
//! - `bounds.rs` carries the `<T: P>` bound enforcement.
//!
//! This file pins the *registry* shape the dispatch path consumes:
//! the same method name on disjoint specialized targets coexists,
//! and disjoint method names on overlapping targets coexist. The
//! collision check is keyed by `[target_head, method_name]`, so
//! two `impl Render for Bag<Int>` and `impl Render for Bag<String>`
//! specializations both register `Bag.render` and are detected as
//! collisions even though their domains are disjoint — the
//! existing diagnostic surface treats this as a shared-method
//! conflict, which is the conservative choice for now.

use expo_ast::util::dedent;

mod common;

use common::{
    diagnostic_messages, typecheck_file as typecheck, typecheck_file_fail as typecheck_fail,
};

#[test]
fn inherent_impl_on_concrete_generic_target_succeeds() {
    // `impl Bag<Int> { fn render(self) -> Int end }` registers a
    // `Bag.render` whose `self` types as `Bag<Int>` only. Calls on
    // `Bag<Int>` succeed.
    let source = "
        struct Bag<T>
          item: T
        end

        extend Bag<Int>
          fn render(self) -> Int
            self.item
          end
        end

        fn use_bag() -> Int
          Bag{item: 1}.render()
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn inherent_impl_on_concrete_generic_diagnoses_mismatched_receiver() {
    // `impl Bag<Int> { fn render(...) end }` does not extend
    // `Bag<String>`. Calling `.render()` on a `Bag<String>` fails.
    let source = "
        struct Bag<T>
          item: T
        end

        extend Bag<Int>
          fn render(self) -> Int
            self.item
          end
        end

        fn use_bag()
          Bag{item: \"x\"}.render()
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("no method `render`") && m.contains("Bag")),
        "expected `no method on receiver` diagnostic, got {messages:?}",
    );
}

#[test]
fn disjoint_method_names_on_specialized_and_generic_targets_coexist() {
    // The "extend"-style model is permissive about *non-overlapping*
    // methods: `impl Bag<Int> { fn render }` and
    // `impl Bag<T> { fn snapshot }` define different methods, so
    // both register cleanly.
    let source = "
        struct Bag<T>
          item: T
        end

        extend Bag<T>
          fn snapshot(self) -> Bag<T>
            self
          end
        end

        extend Bag<Int>
          fn render(self) -> Int
            self.item
          end
        end

        fn use_bag() -> Int
          Bag{item: 1}.render()
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn duplicate_inherent_method_across_impls_diagnoses() {
    // `impl Bag<Int> { fn render }` and `impl Bag<T> { fn render }`
    // both register `[Bag, render]` — the registry rejects the
    // second insertion (collision keyed by target head + method
    // name).
    let source = "
        struct Bag<T>
          item: T
        end

        extend Bag<T>
          fn render(self) -> Int
            0
          end
        end

        extend Bag<Int>
          fn render(self) -> Int
            self.item
          end
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("already defined") && m.contains("Bag.render")),
        "expected duplicate-method diagnostic, got {messages:?}",
    );
}

#[test]
fn distinct_concrete_specializations_share_method_name_diagnose_collision() {
    // `impl Render for Bag<Int>` and `impl Render for Bag<String>`
    // both register `[Bag, render]`. Domains are disjoint, but the
    // current registry keying flags this as a method-name
    // collision. The conservative choice while specialization
    // domains aren't structurally indexed yet — see
    // `StructDefinition`'s `conformances` doc for the transitional
    // note.
    let source = "
        protocol Render
          fn render(self) -> Int
        end

        struct Bag<T>
          item: T
        end

        impl Render for Bag<Int>
          fn render(self) -> Int
            0
          end
        end

        impl Render for Bag<String>
          fn render(self) -> Int
            1
          end
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("already defined") && m.contains("Bag.render")),
        "expected duplicate-method diagnostic for two specialized impls of `Bag.render`, \
         got {messages:?}",
    );
}

#[test]
fn trait_impl_on_concrete_specialization_satisfies_call_site_bounds() {
    // `impl Render for Bag<Int>` makes `Bag<Int>` conform to
    // `Render` (but not `Bag<String>`). A bounded function
    // accepting `T: Render` accepts `Bag<Int>`.
    let source = "
        protocol Render
          fn render(self) -> Int
        end

        struct Bag<T>
          item: T
        end

        impl Render for Bag<Int>
          fn render(self) -> Int
            self.item
          end
        end

        fn show<T: Render>(value: T) -> Int
          value.render()
        end

        fn main
          show(Bag{item: 1})
        end
        ";

    typecheck(&dedent(source));
}
