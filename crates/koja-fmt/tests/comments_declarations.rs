mod common;

use common::*;

#[test]
fn leading_comment_stays_attached_to_declaration() {
    assert_fmt(
        "
        enum A
          B
        end

        # explains P
        priv protocol P
          fn f(self) -> Int
        end
    ",
        "
        enum A
          B
        end

        # explains P
        priv protocol P
          fn f(self) -> Int
        end
    ",
    );
}

#[test]
fn blank_line_before_comment_in_const_run_preserved() {
    assert_unchanged(
        "
        priv const A: Int = 1

        # explains the pair below
        priv const B: Int = 2
        priv const C: Int = 3
    ",
    );
}

#[test]
fn doc_annotated_consts_get_blank_separation() {
    assert_fmt(
        "
        @doc \"\"\"
        The first constant.
        \"\"\"
        const A: Int = 1
        @doc \"\"\"
        The second constant.
        \"\"\"
        const B: Int = 2
        const C: Int = 3
    ",
        "
        @doc \"\"\"
        The first constant.
        \"\"\"
        const A: Int = 1

        @doc \"\"\"
        The second constant.
        \"\"\"
        const B: Int = 2

        const C: Int = 3
    ",
    );
}

#[test]
fn multiple_blank_lines_in_const_run_collapse_to_one() {
    assert_fmt(
        "
        priv const A: Int = 1



        priv const B: Int = 2
    ",
        "
        priv const A: Int = 1

        priv const B: Int = 2
    ",
    );
}

#[test]
fn blank_line_between_comment_and_declaration_preserved() {
    assert_fmt(
        "
        # stray file comment

        fn f -> Int
          1
        end
    ",
        "
        # stray file comment

        fn f -> Int
          1
        end
    ",
    );
}

#[test]
fn comment_above_member_function_stays_above_head() {
    // A comment directly above a fn inside a type body must not be
    // relocated below the signature.
    assert_unchanged(
        "
        struct Wire
          # Pulls a big-endian unsigned 16-bit integer off the front.
          fn take_u16(data: Binary) -> Option<(Int, Binary)>
            Option.None
          end
        end
    ",
    );
}

#[test]
fn comment_above_annotated_impl_function_stays_above_annotation() {
    assert_unchanged(
        "
        impl Display for Point
          # Renders as a coordinate pair.
          @doc \"Human-readable form.\"
          fn display(self) -> String
            \"point\"
          end
        end
    ",
    );
}

#[test]
fn comment_between_annotation_and_declaration_hoists_in_one_pass() {
    assert_fmt(
        "
        @doc \"Adds one.\"
        # explains the function
        fn add_one(x: Int32) -> Int32
          x + 1
        end
    ",
        "
        # explains the function
        @doc \"Adds one.\"
        fn add_one(x: Int32) -> Int32
          x + 1
        end
    ",
    );
    assert_unchanged(
        "
        # explains the function
        @doc \"Adds one.\"
        fn add_one(x: Int32) -> Int32
          x + 1
        end
    ",
    );
}

#[test]
fn comment_between_annotation_and_struct_hoists_instead_of_leaking() {
    assert_fmt(
        "
        @doc \"A point.\"
        # explains the struct
        struct Point
          x: Int32
        end
    ",
        "
        # explains the struct
        @doc \"A point.\"
        struct Point
          x: Int32
        end
    ",
    );
}

#[test]
fn blank_above_annotated_declaration_is_preserved() {
    assert_unchanged(
        "
        # standalone comment

        @doc \"N.\"
        const N = 1
    ",
    );
}

#[test]
fn comment_above_second_member_function_stays_attached() {
    assert_unchanged(
        "
        enum Mode
          Off
          On

          fn flip(self) -> Mode
            Mode.Off
          end

          # Explains the second function.
          fn label(self) -> String
            \"mode\"
          end
        end
    ",
    );
}

#[test]
fn comment_above_protocol_method_stays_above_head() {
    assert_unchanged(
        "
        protocol Marked
          # Marks the value.
          fn mark(self) -> Int

          # Has a default body.
          fn unmark(self) -> Int
            0
          end
        end
    ",
    );
}

#[test]
fn comment_above_impl_type_alias_stays_attached() {
    assert_unchanged(
        "
        impl Container for Bag
          # The element type.
          type Elem = Int

          fn size(self) -> Int
            0
          end
        end
    ",
    );
}

#[test]
fn trailing_comment_before_end_stays_inside_impl_and_protocol() {
    // Matches the struct/enum trailing behavior. The comment attaches
    // directly after the last member.
    assert_unchanged(
        "
        impl Display for Point
          fn display(self) -> String
            \"point\"
          end
          # trailing impl note
        end

        protocol Marked
          fn mark(self) -> Int
          # trailing protocol note
        end
    ",
    );
}

#[test]
fn enum_variant_trailing_comment_stays_on_variant() {
    assert_unchanged(
        "
        enum Signal
          Reload # SIGHUP
          Shutdown # SIGTERM
        end
    ",
    );
}

#[test]
fn enum_struct_variant_field_comments_stay_on_fields() {
    assert_unchanged(
        "
        enum Shape
          Circle{
            # Distance from center to edge.
            radius: Int,
          }
          Rect{
            width: Int, # px
            height: Int,
          }
        end
    ",
    );
}

#[test]
fn fn_header_trailing_comment_stays_on_signature() {
    assert_unchanged(
        "
        fn f(x: Int) -> Int # doubles x
          x * 2
        end
    ",
    );
}

#[test]
fn wrapped_header_trailing_comment_stays_on_signature() {
    assert_fmt(
        "
        fn f(
          x: Int,
        ) -> Int # note
          x
        end
    ",
        "
        fn f(x: Int) -> Int # note
          x
        end
    ",
    );
}

#[test]
fn protocol_method_header_trailing_comment_stays_on_signature() {
    assert_unchanged(
        "
        protocol Marked
          fn mark(self) -> Int # the mark value

          fn unmark(self) -> Int # resets
            0
          end
        end
    ",
    );
}

#[test]
fn alias_const_type_trailing_comments_stay_on_line() {
    assert_unchanged(
        "
        alias Process.Step # step alias

        const N: Int32 = 4 # const trailing

        type Pet = Cat | Dog # type trailing
    ",
    );
}

#[test]
fn declaration_header_and_end_comments_stay_on_line() {
    assert_unchanged(
        "
        struct Point # header comment
          x: Int32
        end # end comment
    ",
    );
}

#[test]
fn test_block_comments_stay_in_place() {
    assert_unchanged(
        r#"
        # leading
        test "with comments" # header
          # inside
          1
          # before end
        end
    "#,
    );
}
