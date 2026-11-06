//! Hashtable key conformances (`src/lower/hash_keys.rs`).
//!
//! The LLVM `Map` / `Set` intrinsics call `<K>.hash/1` and
//! `<K>.equals?/2` by symbol. These tests pin that every key type
//! has both symbols after lowering, with no call site in user code:
//! a conditional impl on a generic struct instantiates, and a union
//! gets one synthesized pair shared by every table keyed on it.

use koja_ir::{FunctionKind, IRInstruction, IRType};

mod common;

use common::{all_instructions, lower_script_source, mangled_function, script_function_names};

const UNION_HASH: &str = "Union_Int64_or_String.hash/1";
const UNION_EQ: &str = "Union_Int64_or_String.equals?/2";

#[test]
fn union_key_synthesizes_regular_hash_and_equals_functions() {
    let source = "
        m: Map<Int | String, Int> = [:]
        m = m.put(1, 1)
        m.get(\"a\")
        ";
    let script = lower_script_source(source);

    let hash = mangled_function(&script, UNION_HASH);
    assert_eq!(hash.kind, FunctionKind::Regular);
    assert_eq!(hash.params.len(), 1);
    assert!(matches!(hash.params[0].ty, IRType::Union { .. }));
    assert_eq!(hash.return_type, IRType::Int64);
    assert!(
        all_instructions(&hash.blocks)
            .any(|instr| matches!(instr, IRInstruction::UnionTagGet { .. })),
        "the synthesized hash should dispatch on the union tag",
    );

    let eq = mangled_function(&script, UNION_EQ);
    assert_eq!(eq.kind, FunctionKind::Regular);
    assert_eq!(eq.params.len(), 2);
    assert_eq!(eq.return_type, IRType::Bool);
}

#[test]
fn map_and_set_on_one_union_share_one_synthesized_pair() {
    let source = "
        m: Map<Int | String, Int> = [:]
        s: Set<Int | String> = Set.new()
        m = m.put(1, 1)
        s = s.insert(\"a\")
        m.length() + s.length()
        ";
    let script = lower_script_source(source);

    let names = script_function_names(&script);
    assert_eq!(names.iter().filter(|name| *name == UNION_HASH).count(), 1);
    assert_eq!(names.iter().filter(|name| *name == UNION_EQ).count(), 1);
}

#[test]
fn conditional_impl_key_instantiates_hash_and_equals_without_a_call_site() {
    let source = "
        struct Pair<A, B>
          a: A
          b: B
        end

        impl Hash for Pair<A: Hash, B: Hash>
          fn hash(self) -> Int
            self.a.hash().bxor(self.b.hash())
          end
        end

        impl Equality for Pair<A: Equality, B: Equality>
          fn equals?(self, other: Pair<A, B>) -> Bool
            self.a == other.a and self.b == other.b
          end
        end

        m: Map<Pair<Int, String>, Int> = [:]
        m = m.put(Pair{a: 1, b: \"x\"}, 7)
        m.length()
        ";
    let script = lower_script_source(source);

    mangled_function(&script, "TestApp.Pair_$Int64.String$.hash/1");
    mangled_function(&script, "TestApp.Pair_$Int64.String$.equals?/2");
}
