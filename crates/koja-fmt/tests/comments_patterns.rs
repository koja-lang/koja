mod common;

use common::*;

#[test]
fn list_pattern_comment_anchors_to_element() {
    assert_fmt(
        "
        fn f(packet: List<Int>) -> Int
          match packet
            [
              # version byte
              version,
              flags
            ] -> version + flags
            _ -> 0
          end
        end
    ",
        "
        fn f(packet: List<Int>) -> Int
          match packet
            [
              # version byte
              version, flags
            ] -> version + flags
            _ -> 0
          end
        end
    ",
    );
}

#[test]
fn tuple_pattern_trailing_comment_stays_on_element() {
    assert_unchanged(
        "
        fn f(point: (Int, Int)) -> Int
          match point
            (
              x, y # unchecked
            ) -> x + y
            _ -> 0
          end
        end
    ",
    );
}

#[test]
fn enum_tuple_pattern_comment_anchors_to_element() {
    assert_unchanged(
        "
        fn f(opt: Option<Int>) -> Int
          match opt
            Option.Some(
              # payload
              value
            ) -> value
            Option.None -> 0
          end
        end
    ",
    );
}

#[test]
fn struct_pattern_field_comments_stay_on_fields() {
    assert_unchanged(
        "
        fn f(shape: Shape) -> Int
          match shape
            Shape.Rect{
              # zero means unbounded
              height: h,
              width: w,
            } -> h * w
            _ -> 0
          end
        end
    ",
    );
}

#[test]
fn binary_pattern_comment_stays_on_segment() {
    assert_unchanged(
        "
        fn f(frame: Binary) -> Int
          match frame
            <<
              header::8, # opcode
              rest::8
            >> -> header + rest
            _ -> 0
          end
        end
    ",
    );
}

#[test]
fn or_pattern_alternative_container_comment_anchors() {
    assert_unchanged(
        "
        fn f(x: List<Int>) -> Int
          match x
            [] | [
              # sentinel
              head
            ] -> 1
            _ -> 0
          end
        end
    ",
    );
}

#[test]
fn commented_pattern_keeps_guard_glued() {
    assert_unchanged(
        "
        fn f(point: (Int, Int)) -> Int
          match point
            (
              x, y # unchecked
            ) when x > 0 -> x
            _ -> 0
          end
        end
    ",
    );
}

#[test]
fn destructure_pattern_comment_anchors_to_element() {
    assert_unchanged_script(
        "
        (
          # sequence counter
          seq, payload
        ) = split(frame)
    ",
    );
}

#[test]
fn for_pattern_comment_anchors_to_element() {
    assert_unchanged(
        "
        fn f(pairs: List<(Int, Int)>) -> Int
          for (
            # coordinate pair
            a, b
          ) in pairs
            a + b
          end

          0
        end
    ",
    );
}
