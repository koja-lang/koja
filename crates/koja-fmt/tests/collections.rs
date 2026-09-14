mod common;

use common::*;

#[test]
fn binary_literal_formatting() {
    assert_fmt_script(
        "
            b = <<1, 2, 3>>
            c = <<header::8, payload::16 big>>
        ",
        "
            b = <<1, 2, 3>>
            c = <<header::8, payload::16 big>>
        ",
    );
}

#[test]
fn long_binary_literal_packs_like_fill() {
    // Segments pack densely like list elements instead of one per
    // line, so a long frame stays a readable byte sequence.
    assert_fmt_script(
        "
            frame = <<0x53::8, 0x51::8, 0x58::8, 0x70::8, 0x50::8, 0x42::8, 0x44::8, 0x45::8, 0x54::8, 0x43::8, 0x5a::8, 0x49::8>>
        ",
        "
            frame = <<
              0x53::8, 0x51::8, 0x58::8, 0x70::8, 0x50::8, 0x42::8, 0x44::8, 0x45::8,
              0x54::8, 0x43::8, 0x5a::8, 0x49::8
            >>
        ",
    );
}

#[test]
fn long_binary_pattern_packs_like_fill() {
    assert_fmt_script(
        "
            match data
              <<first_field::32, second_field::32, third_field::32, fourth_field::16, fifth_field::16, rest: Binary>> -> rest
              _ -> data
            end
        ",
        "
            match data
              <<
                first_field::32, second_field::32, third_field::32, fourth_field::16,
                fifth_field::16, rest: Binary
              >> ->
                rest

              _ ->
                data
            end
        ",
    );
}

/// Segment values and sizes are full expressions. The pattern path once
/// rendered anything past an identifier or literal as `<expr>`.
#[test]
fn binary_pattern_segment_expressions_round_trip() {
    assert_unchanged_script(
        "
            match data
              <<head::len * 8, tag::Header.WIDTH, rest: Binary>> -> rest
              _ -> data
            end
        ",
    );
}

/// A string pattern keeps its `\\#{` escape, otherwise the output would
/// re-parse as an interpolation.
#[test]
fn string_pattern_keeps_interpolation_escape() {
    assert_unchanged_script(
        r#"
            match s
              "\#{name}" -> 1
              _ -> 0
            end
        "#,
    );
}

#[test]
fn concat_operator() {
    assert_fmt_script(
        r#"
            s = "hello" <> " " <> "world"
        "#,
        r#"
            s = "hello" <> " " <> "world"
        "#,
    );
}

#[test]
fn long_concat_chain_packs_like_fill() {
    // A wrapped trailing-operator chain packs operands fill-style
    // instead of one per line after the first break.
    assert_fmt_script(
        r#"
            params = cstring("user") <> cstring(user) <> cstring("database") <> cstring(database) <> terminator
        "#,
        r#"
            params = cstring("user") <> cstring(user) <> cstring("database") <>
              cstring(database) <> terminator
        "#,
    );
}

#[test]
fn struct_construction_short_inline() {
    assert_fmt_script(
        r#"
            c = Config{name: "yo", enabled: true}
        "#,
        r#"
            c = Config{name: "yo", enabled: true}
        "#,
    );
}

#[test]
fn struct_construction_long_multiline() {
    assert_fmt_script(
        r#"
            c = Config{name: "a very long name here", enabled: true, verbose: false, timeout: 3000}
        "#,
        r#"
            c = Config{
              name: "a very long name here",
              enabled: true,
              verbose: false,
              timeout: 3000,
            }
        "#,
    );
}

#[test]
fn ternary_expression() {
    assert_fmt(
        "
            fn f(x: Int) -> Int
              y = x > 0 ? x : -x
              y
            end
        ",
        "
            fn f(x: Int) -> Int
              y = x > 0 ? x : -x
              y
            end
        ",
    );
}

#[test]
fn tuple_syntax_round_trips() {
    assert_unchanged(
        r#"
            fn split(pair: (Int, String)) -> String
              (n, name) = pair

              match (n, name)
                (0, _) -> "zero"
                (_, label) -> label
              end
            end
        "#,
    );
}

#[test]
fn long_tuple_literal_packs_like_fill() {
    assert_fmt_script(
        r#"
            t = (first_element_value, second_element_value, third_element_value, fourth_element_value)
        "#,
        r#"
            t = (
              first_element_value, second_element_value, third_element_value,
              fourth_element_value
            )
        "#,
    );
}

#[test]
fn try_and_fail_round_trip() {
    assert_unchanged(
        "
            fn caller(flag: Bool) -> Int ! MyError
              if not flag
                fail MyError.Nope
              end

              n = try parse(\"1\")
              try parse(\"2\")
            end
        ",
    );
}

#[test]
fn short_rescue_stays_inline() {
    assert_unchanged(
        "
            fn caller(s: String) -> Int
              parse(s) rescue _ -> 0
            end
        ",
    );
}

#[test]
fn long_rescue_breaks_onto_continuation_line() {
    assert_fmt(
        "
            fn connect(config: Config) -> Connection ! Error
              socket = TCPSocket.connect(config.host, config.port) rescue e -> fail Error.ConnectFailed(e.message())
              handshake(socket)
            end
        ",
        "
            fn connect(config: Config) -> Connection ! Error
              socket = TCPSocket.connect(config.host, config.port)
                rescue e -> fail Error.ConnectFailed(e.message())
              handshake(socket)
            end
        ",
    );
}

#[test]
fn interpolation_never_breaks_inside_string() {
    // The interpolation expression renders flat, so the string
    // stays on one long line.
    assert_unchanged_script(
        r#"msg = "the measured value #{measured_value} exceeded the configured threshold #{threshold_value} by #{measured_value - threshold_value}""#,
    );
}
