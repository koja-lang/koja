mod common;

use common::*;

#[test]
fn doc_on_type_alias() {
    assert_fmt(
        "
        @doc \"A user ID.\"
        type UserId = Int
    ",
        "
        @doc \"A user ID.\"
        type UserId = Int
    ",
    );
}

#[test]
fn function_and_constant_aliases_are_canonical() {
    assert_unchanged(
        "
        alias Test.require
        alias JSON.decode as parse
        alias Global.STDOUT as OUT
    ",
    );
}

#[test]
fn nested_protocol_in_both_spellings_is_canonical() {
    assert_fmt(
        "
        struct Date
            day: Int
          protocol   Format
                fn format_date(self,date: Date)->String
          end
        end
        protocol   Date.Parse
          fn parse_date(self, text: String) -> Date
        end
    ",
        "
        struct Date
          day: Int

          protocol Format
            fn format_date(self, date: Date) -> String
          end
        end

        protocol Date.Parse
          fn parse_date(self, text: String) -> Date
        end
    ",
    );
}

#[test]
fn nested_constants_group_apart_from_fields() {
    assert_fmt(
        "
        struct Span
            value: Int
          const   ZERO   =  Span{value: 0}
          priv const limit: Int = 100
          fn zero?(self) -> Bool
            self.value == 0
          end
        end
        const   Span.MAX = Span{value: 9}
        builtin Int
          const MAX = 9223372036854775807
        end
    ",
        "
        struct Span
          value: Int

          const ZERO = Span{value: 0}
          priv const limit: Int = 100

          fn zero?(self) -> Bool
            self.value == 0
          end
        end

        const Span.MAX = Span{value: 9}

        builtin Int
          const MAX = 9223372036854775807
        end
    ",
    );
}

#[test]
fn nested_constants_group_apart_from_variants() {
    assert_fmt(
        "
        enum Direction
          North
          South
          const DEFAULT = Direction.North
          const COUNT = 2
        end
    ",
        "
        enum Direction
          North
          South

          const DEFAULT = Direction.North
          const COUNT = 2
        end
    ",
    );
}

#[test]
fn nested_constants_before_fields_group_apart() {
    assert_fmt(
        "
        struct Slot
          const EMPTY = Slot{ref: Option.None}
          ref: Option<Int>

          size: Int
        end
    ",
        "
        struct Slot
          const EMPTY = Slot{ref: Option.None}

          ref: Option<Int>
          size: Int
        end
    ",
    );
}

#[test]
fn single_annotation_on_function() {
    assert_fmt(
        r#"
        @doc "Adds two numbers."
        fn add(a: Int, b: Int) -> Int
          a + b
        end
    "#,
        r#"
        @doc "Adds two numbers."
        fn add(a: Int, b: Int) -> Int
          a + b
        end
    "#,
    );
}

#[test]
fn stacked_annotations_on_struct() {
    assert_fmt(
        "
        @link \"argon2\"
        @extern \"C\"
        struct Argon2C
          x: Int
        end
    ",
        "
        @link \"argon2\"
        @extern \"C\"
        struct Argon2C
          x: Int
        end
    ",
    );
}

#[test]
fn builtin_empty_body_is_canonical() {
    assert_unchanged(
        "
        @doc \"An immutable UTF-8 string.\"
        builtin String
        end
    ",
    );
}

#[test]
fn builtin_with_functions_is_canonical() {
    assert_unchanged(
        "
        builtin List<T>
          fn length(self) -> Int
            0
          end

          priv fn helper -> Int
            1
          end
        end
    ",
    );
}

#[test]
fn conformance_header_short_is_canonical() {
    assert_unchanged(
        "
        struct Point: Display, Hash
          x: Int

          fn display(self) -> String
            \"point\"
          end
        end
    ",
    );
}

#[test]
fn enum_conformance_header_is_canonical() {
    assert_unchanged(
        "
        enum Color: Display
          Red
          Green

          fn display(self) -> String
            \"color\"
          end
        end
    ",
    );
}

#[test]
fn conformance_header_long_wraps_after_colon() {
    let wrapped = "
        struct Server<T>:
          Process<Config<T>, Msg<T>, Reply<T>>, Serialization, Comparable, Debug

          available: List<T>
        end
    ";
    assert_fmt(
        "
        struct Server<T>: Process<Config<T>, Msg<T>, Reply<T>>, Serialization, Comparable, Debug
          available: List<T>
        end
    ",
        wrapped,
    );
    assert_unchanged(wrapped);
}

#[test]
fn stacked_annotations_on_function() {
    assert_fmt(
        "
        @doc \"Hashes a password.\"
        @test
        fn test_hash
          x = 1
        end
    ",
        "
        @doc \"Hashes a password.\"
        @test
        fn test_hash
          x = 1
        end
    ",
    );
}

#[test]
fn extern_c_function_no_body() {
    assert_fmt(
        "
        @extern \"C\"
        fn argon2id_hash_encoded(t_cost: UInt32, m_cost: UInt32) -> Int32
    ",
        "
        @extern \"C\"
        fn argon2id_hash_encoded(t_cost: UInt32, m_cost: UInt32) -> Int32
    ",
    );
}

#[test]
fn extern_c_struct_per_function() {
    assert_fmt(
        "
        struct Argon2C
          @extern \"C\" @link \"argon2\"
          fn hash_encoded(t_cost: UInt32) -> Int32
          @extern \"C\" @link \"argon2\"
          fn verify(encoded: UInt32) -> Int32
        end
    ",
        "
        struct Argon2C
          @extern \"C\" @link \"argon2\"
          fn hash_encoded(t_cost: UInt32) -> Int32

          @extern \"C\" @link \"argon2\"
          fn verify(encoded: UInt32) -> Int32
        end
    ",
    );
}

#[test]
fn extern_c_multiline_sig_no_double_blank() {
    assert_fmt(
        "
        struct Crypto
          @extern \"C\" @link \"crypto:EVP_DigestInit_ex\"
          priv fn evp_digest_init_ex(ctx: CPtr<UInt8>, md: CPtr<UInt8>, engine: CPtr<UInt8>) -> Int64
          @extern \"C\" @link \"crypto:EVP_DigestUpdate\"
          priv fn evp_digest_update(ctx: CPtr<UInt8>, data: CPtr<UInt8>, len: Int64) -> Int64
          @extern \"C\" @link \"crypto:EVP_MD_CTX_free\"
          priv fn evp_md_ctx_free(ctx: CPtr<UInt8>)
        end
    ",
        "
        struct Crypto
          @extern \"C\" @link \"crypto:EVP_DigestInit_ex\"
          priv fn evp_digest_init_ex(
            ctx: CPtr<UInt8>,
            md: CPtr<UInt8>,
            engine: CPtr<UInt8>,
          ) -> Int64

          @extern \"C\" @link \"crypto:EVP_DigestUpdate\"
          priv fn evp_digest_update(ctx: CPtr<UInt8>, data: CPtr<UInt8>, len: Int64)
            -> Int64

          @extern \"C\" @link \"crypto:EVP_MD_CTX_free\"
          priv fn evp_md_ctx_free(ctx: CPtr<UInt8>)
        end
    ",
    );
}

#[test]
fn priv_decls_round_trip() {
    assert_unchanged(
        "
        priv struct Hidden
          value: Int
        end

        priv enum Mode
          Off
          On
        end

        priv type Pet = Hidden

        priv protocol Marked
          fn mark(self) -> Int
        end

        priv const LIMIT: Int = 10
    ",
    );
}

#[test]
fn conditional_impl_target_bounds_round_trip() {
    assert_unchanged(
        "
        impl Equality for List<T: Equality>
          fn equals?(self, other: List<T>) -> Bool
            true
          end
        end
    ",
    );
    assert_unchanged(
        "
        impl Show for Pair<A: Debug & Hash, B>
          fn show(self) -> String
            \"pair\"
          end
        end
    ",
    );
}

#[test]
fn enum_struct_variant_short_stays_inline_with_hugging_braces() {
    assert_fmt(
        "
        enum Shape
          Circle {
            radius: Int,
          }
          Rect {
            width: Int,
            height: Int = 2,
          }
        end
    ",
        "
        enum Shape
          Circle{radius: Int}
          Rect{width: Int, height: Int = 2}
        end
    ",
    );
}

#[test]
fn enum_struct_variant_long_breaks_with_trailing_commas() {
    assert_unchanged(
        "
        enum Event
          ConnectionEstablished{
            remote_address: String,
            negotiated_protocol_version: Int,
            keepalive_interval_ms: Int,
          }
        end
    ",
    );
}

#[test]
fn top_level_test_block_is_stable() {
    assert_unchanged(
        r#"
        test "push then pop returns the value"
          stack = Stack.new()
          stack.push(1)
          assert stack.pop() == Option.Some(1)
        end
    "#,
    );
}

#[test]
fn empty_test_block_keeps_end_on_its_own_line() {
    assert_fmt(
        r#"
        test "nothing yet" end
    "#,
        r#"
        test "nothing yet"
        end
    "#,
    );
}

#[test]
fn test_description_escapes_round_trip() {
    assert_unchanged(
        r#"
        test "quotes \"inside\" and a tab\t"
          1
        end
    "#,
    );
}

#[test]
fn struct_tests_print_after_functions_with_blank_lines() {
    assert_fmt(
        r#"
        struct Stack
          items: List<Int>
          fn push(self, value: Int) -> Stack
            self
          end
          test "push grows the stack"
            assert Stack.new().push(1).size() == 1
          end
          test "pop shrinks the stack"
          end
        end
    "#,
        r#"
        struct Stack
          items: List<Int>

          fn push(self, value: Int) -> Stack
            self
          end

          test "push grows the stack"
            assert Stack.new().push(1).size() == 1
          end

          test "pop shrinks the stack"
          end
        end
    "#,
    );
}

#[test]
fn type_body_members_keep_source_order() {
    assert_unchanged(
        r#"
        struct Stack
          items: List<Int>

          test "push grows the stack"
            assert Stack.new().push(1).size() == 1
          end

          fn push(self, value: Int) -> Stack
            self
          end

          struct Frame
          end

          @doc "Removes the top item."
          fn pop(self) -> Stack
            self
          end

          test "pop shrinks the stack"
          end
        end

        enum Color
          Red
          Green

          test "red is primary"
            assert Color.Red.primary?()
          end

          fn primary?(self) -> Bool
            true
          end
        end

        impl Display for Color
          test "formats red"
            assert Color.Red.format() == "red"
          end

          fn format(self) -> String
            "red"
          end
        end
    "#,
    );
}

#[test]
fn enum_impl_and_extend_tests_are_stable() {
    assert_unchanged(
        r#"
        enum Color
          Red
          Green

          fn primary?(self) -> Bool
            true
          end

          test "red is primary" # header
            assert Color.Red.primary?()
          end
        end

        impl Display for Color
          fn format(self) -> String
            "red"
          end

          test "formats red"
            assert Color.Red.format() == "red"
          end
        end

        extend List<Int>
          fn total(self) -> Int
            0
          end

          test "total of empty is zero"
            assert [].total() == 0
          end
        end
    "#,
    );
}
