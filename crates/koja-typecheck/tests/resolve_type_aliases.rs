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
//! - A type declaration of a union takes the members of each type
//!   declaration of a union that it names, in each order of the two.
//!   A union inside a type argument, a tuple, or a function type does
//!   the same.
//! - A type declaration that is a member of a union in its own
//!   definition surfaces a cycle diagnostic at that member.
//! - The same alias is exhaustiveness-aware in `match`: a typed-
//!   binding arm hits each member of the underlying union, missing
//!   members surface a clean diagnostic.

use koja_ast::identifier::{AnonymousKind, ResolvedType};
use koja_ast::util::dedent;
use koja_parser::ParseMode;
use koja_typecheck::CheckedProgram;

mod common;

use common::{
    PACKAGE, assert_file_fails_with, bool_type, check_packages, diagnostic_messages, float_type,
    global_named, int_type, package_leaf, registry_id, string_type, typecheck_file as typecheck,
    typecheck_file_fail as typecheck_fail,
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

/// The expansion that the registry holds for the type declaration
/// `name` of the test package.
fn expansion_of(checked: &CheckedProgram, name: &str) -> ResolvedType {
    let id = registry_id(checked, PACKAGE, &[name]);
    checked
        .registry
        .alias_expansion(id)
        .unwrap_or_else(|| panic!("type `{name}` has no expansion"))
}

#[test]
fn union_type_declaration_takes_the_members_of_a_later_one() {
    let source = "
        type B = Int | A
        type A = String | Bool

        fn kind(b: B) -> String
          match b
            _n: Int -> \"int\"
            _s: String -> \"string\"
            _f: Bool -> \"bool\"
          end
        end

        fn main -> String
          text: B = \"s\"
          flag: A = true
          wide: B = flag
          \"#{kind(text)} #{kind(wide)}\"
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn union_type_declaration_chain_flattens_at_each_forward_step() {
    let source = "
        type C = Float | B
        type B = Int | A
        type A = String | Bool

        fn main -> C
          text: C = \"s\"
          text
        end
        ";
    let checked = typecheck(&dedent(source));
    let expected = ResolvedType::Anonymous(AnonymousKind::Union {
        members: vec![
            bool_type(&checked),
            float_type(&checked),
            int_type(&checked),
            string_type(&checked),
        ],
    });
    assert_eq!(expansion_of(&checked, "C"), expected);
}

#[test]
fn union_type_declaration_is_one_type_in_each_order() {
    let later = "
        type B = Int | A
        type A = String | Bool
        ";
    let earlier = "
        type A = String | Bool
        type B = Int | A
        ";
    for source in [later, earlier] {
        let checked = typecheck(&dedent(source));
        let expected = ResolvedType::Anonymous(AnonymousKind::Union {
            members: vec![
                bool_type(&checked),
                int_type(&checked),
                string_type(&checked),
            ],
        });
        assert_eq!(expansion_of(&checked, "B"), expected, "source: {source}");
    }
}

#[test]
fn union_type_declaration_takes_the_members_of_a_later_recursive_one() {
    let source = "
        type V = Int | W
        type W = String | List<V>

        fn size(v: V) -> Int
          match v
            _n: Int -> 1
            _s: String -> 1
            items: List<V> ->
              items.reduce(0, fn (sum: Int, item: V) -> Int sum + size(item) end)
          end
        end

        fn main -> Int
          text: V = \"s\"
          w: W = [1, \"two\", [3, \"four\"]]
          v: V = w
          size(text) + size(v)
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn recursive_type_declaration_pair_is_one_type_in_each_order() {
    let later = "
        type V = Int | W
        type W = String | List<V>
        ";
    let earlier = "
        type W = String | List<V>
        type V = Int | W
        ";
    for source in [later, earlier] {
        let checked = typecheck(&dedent(source));
        let expected = ResolvedType::Anonymous(AnonymousKind::Union {
            members: vec![
                int_type(&checked),
                global_named(&checked, "List", vec![package_leaf(&checked, "V")]),
                string_type(&checked),
            ],
        });
        assert_eq!(expansion_of(&checked, "V"), expected, "source: {source}");
        // `V` names itself in `List<V>`. The union of `V` holds the
        // members of `W`, so `W` names `V` and never itself.
        let is_recursive = |name: &str| {
            let id = registry_id(&checked, PACKAGE, &[name]);
            checked.registry.is_recursive_alias(id)
        };
        assert!(is_recursive("V"), "source: {source}");
        assert!(!is_recursive("W"), "source: {source}");
    }
}

#[test]
fn union_type_declaration_collapses_with_a_later_type_declaration_of_a_member() {
    let source = "
        type B = Int | A
        type A = Int

        fn twice(b: B) -> Int
          b * 2
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(expansion_of(&checked, "B"), int_type(&checked));
}

#[test]
fn union_type_declaration_cycle_of_two_diagnoses_both() {
    let source = "
        type A = Int | B
        type B = String | A
        ";
    assert_file_fails_with(
        source,
        &[
            "type alias `TestApp.A` references itself (cycle)",
            "type alias `TestApp.B` references itself (cycle)",
        ],
    );
}

/// A union of `members`, which must be in canonical order.
fn union_of(members: Vec<ResolvedType>) -> ResolvedType {
    ResolvedType::Anonymous(AnonymousKind::Union { members })
}

/// The two declaration orders of `first` and `second`.
fn each_order(first: &str, second: &str) -> [String; 2] {
    [
        format!("{first}\n{second}\n"),
        format!("{second}\n{first}\n"),
    ]
}

/// Assert that the type declaration `name` has the expansion
/// `expected` in each order of the declarations `first` and `second`.
fn assert_expansion_in_each_order(
    first: &str,
    second: &str,
    name: &str,
    expected: impl Fn(&CheckedProgram) -> ResolvedType,
) {
    for source in each_order(first, second) {
        let checked = typecheck(&source);
        assert_eq!(
            expansion_of(&checked, name),
            expected(&checked),
            "source: {source}"
        );
    }
}

/// Assert that each order of the declarations `first` and `second`
/// fails with a diagnostic for each text of `needles`.
fn assert_fails_in_each_order(first: &str, second: &str, needles: &[&str]) {
    for source in each_order(first, second) {
        assert_file_fails_with(&source, needles);
    }
}

#[test]
fn union_in_a_list_argument_takes_the_members_of_a_type_declaration_in_each_order() {
    assert_expansion_in_each_order(
        "type B = List<Int | A>",
        "type A = String | Bool",
        "B",
        |checked| {
            let element = union_of(vec![
                bool_type(checked),
                int_type(checked),
                string_type(checked),
            ]);
            global_named(checked, "List", vec![element])
        },
    );
}

#[test]
fn union_in_a_tuple_element_takes_the_members_of_a_type_declaration_in_each_order() {
    assert_expansion_in_each_order(
        "type B = (Int | A, String)",
        "type A = String | Bool",
        "B",
        |checked| {
            let first = union_of(vec![
                bool_type(checked),
                int_type(checked),
                string_type(checked),
            ]);
            ResolvedType::Anonymous(AnonymousKind::Tuple {
                elements: vec![first, string_type(checked)],
            })
        },
    );
}

#[test]
fn union_in_a_function_parameter_takes_the_members_of_a_type_declaration_in_each_order() {
    assert_expansion_in_each_order(
        "type B = fn (Int | A) -> Int",
        "type A = String | Bool",
        "B",
        |checked| {
            let param = union_of(vec![
                bool_type(checked),
                int_type(checked),
                string_type(checked),
            ]);
            ResolvedType::Anonymous(AnonymousKind::Function {
                params: vec![param],
                ret: Box::new(int_type(checked)),
            })
        },
    );
}

#[test]
fn union_in_an_argument_of_a_member_takes_the_members_of_a_type_declaration_in_each_order() {
    assert_expansion_in_each_order(
        "type B = Int | List<Float | A>",
        "type A = String | Bool",
        "B",
        |checked| {
            let element = union_of(vec![
                bool_type(checked),
                float_type(checked),
                string_type(checked),
            ]);
            union_of(vec![
                int_type(checked),
                global_named(checked, "List", vec![element]),
            ])
        },
    );
}

#[test]
fn union_in_a_list_argument_takes_the_members_of_a_recursive_type_declaration_in_each_order() {
    assert_expansion_in_each_order(
        "type W = String | List<Float | V>",
        "type V = Int | List<V>",
        "W",
        |checked| {
            let element = union_of(vec![
                float_type(checked),
                int_type(checked),
                global_named(checked, "List", vec![package_leaf(checked, "V")]),
            ]);
            union_of(vec![
                global_named(checked, "List", vec![element]),
                string_type(checked),
            ])
        },
    );
}

#[test]
fn nested_union_takes_the_members_of_a_type_declaration_chain_in_each_order() {
    let b = "type B = List<Int | A>";
    let a = "type A = String | C";
    let c = "type C = Bool | Float";
    // In the second order, `B` takes the members of `A` before `A`
    // has the members of `C`.
    for order in [[b, a, c], [a, b, c], [c, a, b]] {
        let source = format!("{}\n", order.join("\n"));
        let checked = typecheck(&source);
        let element = union_of(vec![
            bool_type(&checked),
            float_type(&checked),
            int_type(&checked),
            string_type(&checked),
        ]);
        let expected = global_named(&checked, "List", vec![element]);
        assert_eq!(expansion_of(&checked, "B"), expected, "source: {source}");
    }
}

#[test]
fn recursive_type_declaration_with_a_named_inner_union_is_one_type_in_each_order() {
    let outer = "type T = Int | List<Item>";
    let inner = "type Item = Float | T";
    let items = |checked: &CheckedProgram| {
        global_named(checked, "List", vec![package_leaf(checked, "Item")])
    };
    assert_expansion_in_each_order(outer, inner, "Item", |checked| {
        union_of(vec![float_type(checked), int_type(checked), items(checked)])
    });
    assert_expansion_in_each_order(outer, inner, "T", |checked| {
        union_of(vec![int_type(checked), items(checked)])
    });
}

#[test]
fn type_declaration_in_a_nested_union_of_its_own_definition_diagnoses_at_the_member() {
    let source = "type T = Int | List<Float | T>\n";
    let failure = typecheck_fail(source);
    let [diagnostic] = failure.diagnostics.as_slice() else {
        panic!(
            "expected one diagnostic, got {:#?}",
            diagnostic_messages(&failure)
        );
    };
    assert_eq!(
        diagnostic.message,
        "type `TestApp.T` is a member of a union in its own definition (cycle)"
    );
    assert_eq!(
        diagnostic.hint.as_deref(),
        Some("give the union inside the type argument a type declaration of its own")
    );
    // The span is the `T` inside the list, and not the declaration.
    let member = source.rfind('T').expect("the source names `T`");
    assert_eq!(diagnostic.span.start.offset as usize, member);
    assert_eq!(diagnostic.span.end.offset as usize, member + 1);
}

#[test]
fn two_type_declarations_in_nested_unions_of_each_other_diagnose_both() {
    assert_fails_in_each_order(
        "type A = Int | List<String | B>",
        "type B = Float | List<Bool | A>",
        &[
            "type `TestApp.A` is a member of a union in its own definition through \
             `TestApp.B` (cycle)",
            "type `TestApp.B` is a member of a union in its own definition through \
             `TestApp.A` (cycle)",
        ],
    );
}

#[test]
fn type_declaration_cycle_through_a_direct_member_and_a_nested_union_diagnoses_both() {
    assert_fails_in_each_order(
        "type A = Int | B",
        "type B = String | List<Float | A>",
        &[
            "type `TestApp.A` is a member of a union in its own definition through \
             `TestApp.B` (cycle)",
            "type `TestApp.B` is a member of a union in its own definition through \
             `TestApp.A` (cycle)",
        ],
    );
}

#[test]
fn type_declaration_cycle_through_a_declaration_of_a_bare_name_diagnoses_both() {
    assert_fails_in_each_order(
        "type T = Int | List<Float | U>",
        "type U = T",
        &[
            "type `TestApp.T` is a member of a union in its own definition through \
             `TestApp.U` (cycle)",
            "type `TestApp.U` is a member of a union in its own definition through \
             `TestApp.T` (cycle)",
        ],
    );
}

#[test]
fn type_declaration_that_reads_a_cycle_gets_no_diagnostic_of_its_own() {
    let source = "type T = Int | List<Float | T>\ntype U = String | T\n";
    let failure = typecheck_fail(source);
    assert_eq!(
        diagnostic_messages(&failure),
        ["type `TestApp.T` is a member of a union in its own definition (cycle)"]
    );
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
