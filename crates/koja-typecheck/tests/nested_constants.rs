//! Typecheck coverage for constants nested under a type, in both
//! spellings (`const Span.ZERO = ...` at the top level, `const ZERO`
//! inside the owner's body). A nested constant reads as `Span.ZERO`
//! from anywhere, including the owner's own body, and serves as a
//! field default. The bare name never resolves, matching nested
//! types.

use koja_ast::util::dedent;

mod common;

use common::{assert_script_fails_with, typecheck_script as typecheck};

#[test]
fn nested_constants_read_in_every_position() {
    typecheck(&dedent(
        "
        struct Span
          value: Int
          unit: Span.Unit

          enum Unit
            Seconds
            Minutes
          end

          const ZERO = Span{value: 0, unit: Span.Unit.Seconds}
          const limit = 100

          fn zero?(self) -> Bool
            self.value == Span.ZERO.value
          end
        end

        const Span.MAX = Span{value: 99, unit: Span.Unit.Minutes}

        enum Color
          Red
          Green

          const DEFAULT = Color.Green
        end

        fn take(s: Span) -> Int
          s.value
        end

          a: Int = take(Span.ZERO) + take(Span.MAX) + Span.limit
          c: Color = Color.DEFAULT
          Span.ZERO.zero?()
        ",
    ));
}

#[test]
fn nested_constant_is_a_field_default() {
    typecheck(&dedent(
        "
        struct Span
          value: Int

          const ZERO = Span{value: 0}
          const limit = 100
        end

        enum Color
          Red
          Green

          const DEFAULT = Color.Green
        end

        struct Summary
          elapsed: Span = Span.ZERO
          count: Int = Span.limit
          color: Color = Color.DEFAULT
        end

          s = Summary{}
          s.elapsed.value + s.count
        ",
    ));
}

#[test]
fn constant_value_constructs_a_nested_struct() {
    // `TimeZone.Offset{...}` parses as a struct-shaped variant of
    // `TimeZone`. The constant lifter must rewrite it to a struct
    // construction the way the body resolver does.
    typecheck(&dedent(
        "
        enum TimeZone
          UTC
          Fixed(TimeZone.Offset)

          struct Offset
            seconds: Int

            const UTC = TimeZone.Offset{seconds: 0}
          end
        end

          TimeZone.Fixed(TimeZone.Offset.UTC)
        ",
    ));
}

#[test]
fn package_constant_is_a_field_default() {
    typecheck(&dedent(
        "
        const LIMIT = 10

        struct Config
          limit: Int = LIMIT
        end

          Config{}.limit
        ",
    ));
}

#[test]
fn bare_name_does_not_resolve_inside_owner() {
    assert_script_fails_with(
        "
        struct Span
          value: Int

          const ZERO = Span{value: 0}

          fn zero?(self) -> Bool
            self.value == ZERO.value
          end
        end
        ",
        &["unknown identifier `ZERO`"],
    );
}

#[test]
fn nested_constant_requires_known_owner_in_same_package() {
    assert_script_fails_with(
        "
        const Missing.MAX = 1
        ",
        &["nested constant `MAX` must be declared under a type in the same package"],
    );
}

#[test]
fn nested_constant_cannot_shadow_enum_variant() {
    assert_script_fails_with(
        "
        enum Color
          Red

          const Red = 1
        end
        ",
        &["nested constant `Red` collides with a variant of `Color`"],
    );
}

#[test]
fn nested_constant_cannot_share_a_method_name() {
    assert_script_fails_with(
        "
        struct Span
          value: Int

          const zero = 0

          fn zero(self) -> Int
            0
          end
        end
        ",
        &["`TestApp.Span.zero` is already defined"],
    );
}

#[test]
fn unknown_struct_member_error_mentions_constants_only() {
    assert_script_fails_with(
        "
        struct Span
          value: Int
        end

          Span.MAX
        ",
        &["`TestApp.Span` has no constant `MAX` (it is a struct, not an enum)"],
    );
}

#[test]
fn unknown_enum_member_error_mentions_variants_and_constants() {
    assert_script_fails_with(
        "
        enum Color
          Red
        end

          Color.Blue
        ",
        &["`TestApp.Color` has no variant or constant `Blue`"],
    );
}
