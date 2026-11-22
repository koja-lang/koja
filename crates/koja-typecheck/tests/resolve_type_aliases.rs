//! Top-level `type X = T` aliases, the surface companion to the
//! file-level `alias` slice (which lives in `tests/aliases.rs` and
//! covers cross-package re-exports).
//!
//! `type X = T` registers `X` as a [`crate::registry::GlobalKind::TypeAlias`]
//! entry in the current package. Resolution at use sites returns a
//! `ResolvedType::Named { resolution, .. }` that points at the alias
//! identifier so diagnostics keep the user's name. Equivalence
//! peels through the alias when comparing types, so `Pet` ≡
//! `Cat | Dog | Fish` for `types_equivalent` purposes.
//!
//! Pinned shapes:
//!
//! - Alias to a union resolves at a use site (param / return / let
//!   slot) and accepts every member as a widening source.
//! - Alias display name round-trips through diagnostics: a
//!   mismatch reports the alias, not the expanded union.
//! - Aliases are reachable cross-package (the registry promotes
//!   them to package-global entries).
//! - Self-referential aliases (`type X = X | A`) surface a precise
//!   cycle diagnostic.
//! - A union alias can name itself in a type argument of `List`,
//!   `Map`, `Set`, or a generic struct or enum. Such an alias is a
//!   type with a name of its own.
//! - The same alias is exhaustiveness-aware in `match`: a typed-
//!   binding arm hits each member of the underlying union, missing
//!   members surface a clean diagnostic.

use koja_ast::util::dedent;
use koja_parser::ParseMode;

mod common;

use common::{
    PACKAGE, assert_file_fails_with, check_packages, diagnostic_messages,
    typecheck_file as typecheck, typecheck_file_fail as typecheck_fail,
};

#[test]
fn type_alias_to_struct_resolves_at_use_site() {
    let source = "
        struct Cat
          name: String
        end

        type Feline = Cat

        fn rename(c: Feline) -> Feline
          c
        end

        fn main -> Feline
          rename(Cat{name: \"whiskers\"})
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn type_alias_to_union_resolves_at_use_site() {
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

        fn describe(pet: Pet) -> String
          match pet
            _ -> \"a pet\"
          end
        end

        fn main -> String
          describe(Cat{name: \"whiskers\"})
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn type_alias_widens_member_at_call_site() {
    // Passing a bare `Cat` into a `Pet` slot widens through the
    // alias's underlying union: `check_compatible` peels the
    // alias before checking member-of-union, so the alias is
    // semantically transparent to coercion.
    let source = "
        struct Cat
          name: String
        end

        struct Dog
          name: String
        end

        type Pet = Cat | Dog

        fn name_of(p: Pet) -> String
          match p
            c: Cat -> c.name
            d: Dog -> d.name
          end
        end

        fn main -> String
          name_of(Cat{name: \"whiskers\"})
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn type_alias_widens_member_in_type_param_slot() {
    // A receiver seeded with `K -> Key` and an arg that binds
    // `K -> Int` is a widening, not a conflict. `Substitution::set`
    // peels the alias before its union-member check.
    let source = "
        type Key = Int | String

        fn lookup(table: Map<Key, Int>) -> Option<Int>
          table.put(7, 1).get(\"seven\")
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn type_alias_preserves_name_in_mismatch_diagnostics() {
    // The mismatch diagnostic reports the alias (`Pet`), not the
    // expanded `Cat | Dog`. This is the headline contract for the
    // user-facing display path: alias users see their alias.
    let source = "
        struct Cat
          name: String
        end

        struct Dog
          name: String
        end

        struct Mouse
          color: String
        end

        type Pet = Cat | Dog

        fn name_of(p: Pet) -> String
          match p
            c: Cat -> c.name
            d: Dog -> d.name
          end
        end

        fn main -> String
          name_of(Mouse{color: \"grey\"})
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("Pet") && !m.contains("Cat | Dog")),
        "expected diagnostic to keep the alias name `Pet` (and not expand to \
         `Cat | Dog`), got {messages:?}",
    );
}

#[test]
fn type_alias_member_typed_binding_arms_resolve() {
    // The match driver peels the alias before computing union
    // exhaustiveness, so typed-binding arms over a `Pet`-typed
    // subject narrow to each underlying member.
    let source = "
        struct Cat
          name: String
        end

        struct Dog
          name: String
        end

        type Pet = Cat | Dog

        fn name_of(p: Pet) -> String
          match p
            c: Cat -> c.name
            d: Dog -> d.name
          end
        end

        fn main -> String
          name_of(Cat{name: \"whiskers\"})
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn type_alias_match_missing_member_diagnoses() {
    // Exhaustiveness names the missing member by its surface type
    // (here `Dog`): alias peeling threads through the missing-
    // member walk, so the user sees a member of the union, not the
    // alias name "Pet".
    let source = "
        struct Cat
          name: String
        end

        struct Dog
          name: String
        end

        type Pet = Cat | Dog

        fn name_of(p: Pet) -> String
          match p
            c: Cat -> c.name
          end
        end

        fn main -> String
          name_of(Cat{name: \"whiskers\"})
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("Dog") && m.to_lowercase().contains("missing")),
        "expected missing-member diagnostic mentioning `Dog`, got {messages:?}",
    );
}

#[test]
fn self_referential_type_alias_diagnoses_cycle() {
    let source = "
        struct A
          x: Int
        end

        type Loop = Loop | A

        fn main -> Int
          0
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.to_lowercase().contains("cycle") || m.to_lowercase().contains("recursive")),
        "expected alias-cycle diagnostic, got {messages:?}",
    );
}

#[test]
fn recursive_alias_resolves_through_list_and_map_arguments() {
    let source = "
        type Value = Bool | Int | String | List<Value> | Map<String, Value>

        fn count(value: Value) -> Int
          match value
            items: List<Value> ->
              items.reduce(0, fn (total: Int, item: Value) -> Int total + count(item) end)
            _fields: Map<String, Value> -> 1
            _ -> 1
          end
        end

        fn main -> Int
          fields: Map<String, Value> = Map.new()
          nested: Value = [1, \"two\", [true, 3], fields]
          count(nested)
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn recursive_alias_resolves_through_generic_struct_and_enum_arguments() {
    let source = "
        struct Box<T>
          value: T
        end

        type Tree = Int | Box<Tree> | Option<Tree>

        fn depth(tree: Tree) -> Int
          match tree
            _n: Int -> 0
            box: Box<Tree> -> 1 + depth(box.value)
            opt: Option<Tree> ->
              match opt
                Option.Some(inner) -> 1 + depth(inner)
                Option.None -> 1
              end
          end
        end

        fn main -> Int
          leaf: Tree = 3
          box: Box<Tree> = Box{value: leaf}
          inner: Tree = box
          opt: Option<Tree> = Option.Some(inner)
          depth(opt)
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn mutually_recursive_aliases_resolve() {
    let source = "
        type A = Int | List<B>
        type B = String | List<A>

        fn size_a(a: A) -> Int
          match a
            _n: Int -> 1
            items: List<B> ->
              items.reduce(0, fn (sum: Int, item: B) -> Int sum + size_b(item) end)
          end
        end

        fn size_b(b: B) -> Int
          match b
            _s: String -> 1
            items: List<A> ->
              items.reduce(0, fn (sum: Int, item: A) -> Int sum + size_a(item) end)
          end
        end

        fn main -> Int
          bs: List<B> = [\"x\", [1, 2, [\"y\", \"z\"]]]
          size_a(bs)
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn alias_cycle_through_a_tuple_member_diagnoses_cycle() {
    let source = "
        type Pair = Int | (Pair, Int)
        ";
    assert_file_fails_with(
        source,
        &["type alias `TestApp.Pair` references itself (cycle)"],
    );
}

#[test]
fn alias_cycle_through_a_function_type_diagnoses_cycle() {
    let source = "
        type Callback = Int | fn (Callback) -> Int
        ";
    assert_file_fails_with(
        source,
        &["type alias `TestApp.Callback` references itself (cycle)"],
    );
}

#[test]
fn recursive_alias_that_is_not_a_union_diagnoses() {
    let source = "
        type Nest = List<Nest>
        ";
    assert_file_fails_with(
        source,
        &["recursive type alias `TestApp.Nest` must be a union"],
    );
}

#[test]
fn two_recursive_aliases_with_one_shape_are_different_types() {
    let source = "
        type A = Int | List<A>
        type B = Int | List<B>

        fn take(b: B) -> Int
          1
        end

        fn main -> Int
          a: A = [1, [2]]
          take(a)
        end
        ";
    assert_file_fails_with(source, &["expects `TestApp.B`, got `TestApp.A`"]);
}

#[test]
fn recursive_aliases_one_step_apart_compare_without_a_loop() {
    // Each union names its alias two levels down. A walk of one
    // union against the other never meets both names at one time.
    let source = "
        type V = Int | List<Int | List<V>>
        type X = Int | List<Int | List<X>>

        fn take(s: Int | List<X>) -> Int
          1
        end

        fn main -> Int
          v: V = 1
          take(v)
        end
        ";
    assert_file_fails_with(
        source,
        &["expects `Int | List<TestApp.X>`, got `TestApp.V`"],
    );
}

#[test]
fn recursive_alias_match_missing_member_diagnoses() {
    let source = "
        type Value = Bool | Int | List<Value>

        fn kind(value: Value) -> String
          match value
            _b: Bool -> \"bool\"
            _n: Int -> \"int\"
          end
        end
        ";
    assert_file_fails_with(
        source,
        &["match is not exhaustive", "`List<TestApp.Value>`"],
    );
}

#[test]
fn recursive_alias_satisfies_equality_when_every_member_does() {
    let source = "
        type Value = Bool | Int | String | List<Value>

        fn equal<T: Equality>(left: T, right: T) -> Bool
          left.equals?(right)
        end

        fn main -> Bool
          a: Value = [1, \"two\", [true, 3]]
          b: Value = [1, \"two\", [true, 3]]
          equal(a, b) and a == b
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn recursive_alias_does_not_satisfy_a_bound_that_a_member_fails() {
    let source = "
        protocol Marked
          fn mark(self) -> Int
        end

        type Value = Int | List<Value>

        fn use_mark<T: Marked>(value: T) -> Int
          value.mark()
        end

        fn reject(value: Value) -> Int
          use_mark(value)
        end
        ";
    assert_file_fails_with(source, &["does not implement protocol `TestApp.Marked`"]);
}

#[test]
fn type_alias_is_visible_cross_package() {
    // `pets.koja` declares `type Pet = Cat | Dog`. `app.koja` in a
    // sibling package brings `Pets.Pet` and `Pets.Cat` into scope
    // via `alias` and uses the alias as a type. The registry
    // promotes `type` aliases to package-global entries so the
    // alias machinery finds them just like it finds structs and
    // enums. Without that promotion the `alias Pets.Pet` line
    // would diagnose with "alias target is not a registered type."
    let result = check_packages(
        &[
            (
                "Pets",
                "pets.koja",
                "struct Cat\n  name: String\nend\n\
             struct Dog\n  name: String\nend\n\
             type Pet = Cat | Dog\n",
            ),
            (
                PACKAGE,
                "app.koja",
                "alias Pets.Cat\n\
             alias Pets.Pet\n\
             fn label(p: Pet) -> String\n  \
                match p\n    \
                  _ -> \"pet\"\n  \
                end\n\
              end\n\
              fn main -> String\n  \
                label(Cat{name: \"whiskers\"})\n\
              end\n",
            ),
        ],
        ParseMode::File,
    );
    assert!(
        result.is_ok(),
        "expected cross-package alias to resolve, got {:?}",
        result.err().map(|f| diagnostic_messages(&f)),
    );
}

#[test]
fn tuple_alias_chain_supports_methods_patterns_and_destructuring() {
    let source = "
        type Pair = (Int, String)
        type NamedPair = Pair

        fn use_pair(pair: NamedPair) -> Bool
          pair.print()
          pair.format()
          (left, _) = pair
          left.print()
          match pair
            (number, label) -> pair.equals?((number, label))
          end
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn tuple_alias_satisfies_structural_protocol_bounds() {
    let source = "
        type Pair = (Int, String)

        fn render<T: Debug>(value: T) -> String
          value.format()
        end

        fn equal<T: Equality>(left: T, right: T) -> Bool
          left.equals?(right)
        end

        fn use_pair(pair: Pair) -> Bool
          render(pair)
          equal(pair, (1, \"one\"))
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn tuple_alias_does_not_satisfy_custom_protocol_bound() {
    let source = "
        protocol Marked
          fn mark(self) -> Int
        end

        type Pair = (Int, Int)

        fn use_mark<T: Marked>(value: T) -> Int
          value.mark()
        end

        fn reject(pair: Pair) -> Int
          use_mark(pair)
        end
        ";

    assert_file_fails_with(
        source,
        &[
            "does not implement protocol `TestApp.Marked`",
            "required by type parameter `T`",
        ],
    );
}
