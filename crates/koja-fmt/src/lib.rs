pub mod doc;
pub mod printer;

use doc::render;
use koja_ast::ast::Diagnostic;
use koja_parser::ParseMode;

/// The result of formatting a source string.
pub enum FormatResult {
    /// Successfully formatted source code.
    Ok(String),
    /// The source could not be parsed; contains the parse diagnostics.
    ParseErrors(Vec<Diagnostic>),
}

/// Formats Koja source code using the default line width (80 columns).
///
/// `mode` selects the top-level grammar: [`ParseMode::Script`] for
/// `.kojs` scripts (top-level statements), [`ParseMode::File`] for
/// `.koja` modules. Callers typically derive it via
/// [`ParseMode::for_path`].
pub fn format(source: &str, mode: ParseMode) -> FormatResult {
    format_width(source, 80, mode)
}

/// Formats Koja source code, wrapping lines at `width` columns.
pub fn format_width(source: &str, width: u32, mode: ParseMode) -> FormatResult {
    let result = koja_parser::parse(source, mode);
    if !result.errors.is_empty() {
        return FormatResult::ParseErrors(result.errors);
    }

    let doc = printer::file_to_doc(&result.ast);
    let rendered = render(&doc, width);
    let mut out: String = rendered
        .lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n");

    if !out.ends_with('\n') {
        out.push('\n');
    }

    FormatResult::Ok(out)
}

#[cfg(test)]
mod tests {
    use koja_ast::util::dedent;

    use super::*;

    fn fmt(source: &str) -> String {
        match format(&dedent(source), ParseMode::File) {
            FormatResult::Ok(s) => s,
            FormatResult::ParseErrors(e) => panic!("parse error: {:?}", e),
        }
    }

    fn assert_fmt(input: &str, expected: &str) {
        let actual = fmt(input);
        let mut expected = dedent(expected);
        if !expected.ends_with('\n') {
            expected.push('\n');
        }
        assert_eq!(
            actual, expected,
            "\n--- actual ---\n{actual}--- expected ---\n{expected}"
        );
    }

    #[test]
    fn or_pattern_short_stays_inline() {
        assert_fmt(
            "
            fn f(x: Int) -> Int
              match x
                1 | 2 | 3 -> 0
                _ -> x
              end
            end
        ",
            "
            fn f(x: Int) -> Int
              match x
                1 | 2 | 3 -> 0
                _ -> x
              end
            end
        ",
        );
    }

    #[test]
    fn or_pattern_long_wraps_with_trailing_pipe() {
        assert_fmt(
            r#"
            fn f(s: String) -> Bool
              match s
                "a" | "b" | "c" | "d" | "e" | "f" | "g" | "h" | "i" | "j" | "k" | "l" | "m" | "n" | "o" | "p" | "q" | "r" | "s" | "t" | "u" | "v" | "w" | "x" | "y" | "z" -> true
                _ -> false
              end
            end
        "#,
            r#"
            fn f(s: String) -> Bool
              match s
                "a" | "b" | "c" | "d" | "e" | "f" | "g" | "h" | "i" | "j" | "k" | "l" |
                "m" | "n" | "o" | "p" | "q" | "r" | "s" | "t" | "u" | "v" | "w" | "x" |
                "y" | "z" ->
                  true

                _ ->
                  false
              end
            end
        "#,
        );
    }

    #[test]
    fn or_pattern_multiline_arms_have_blank_lines() {
        assert_fmt(
            r#"
            fn f(s: String) -> Bool
              match s
                "a" | "b" | "c" | "d" | "e" | "f" | "g" | "h" | "i" | "j" | "k" | "l" | "m" | "n" | "o" | "p" | "q" | "r" | "s" | "t" | "u" | "v" | "w" | "x" | "y" | "z" -> true
                "A" | "B" | "C" | "D" | "E" | "F" | "G" | "H" | "I" | "J" | "K" | "L" | "M" | "N" | "O" | "P" | "Q" | "R" | "S" | "T" | "U" | "V" | "W" | "X" | "Y" | "Z" -> true
                _ -> false
              end
            end
        "#,
            r#"
            fn f(s: String) -> Bool
              match s
                "a" | "b" | "c" | "d" | "e" | "f" | "g" | "h" | "i" | "j" | "k" | "l" |
                "m" | "n" | "o" | "p" | "q" | "r" | "s" | "t" | "u" | "v" | "w" | "x" |
                "y" | "z" ->
                  true

                "A" | "B" | "C" | "D" | "E" | "F" | "G" | "H" | "I" | "J" | "K" | "L" |
                "M" | "N" | "O" | "P" | "Q" | "R" | "S" | "T" | "U" | "V" | "W" | "X" |
                "Y" | "Z" ->
                  true

                _ ->
                  false
              end
            end
        "#,
        );
    }

    #[test]
    fn cond_chain_with_not() {
        assert_fmt("
            fn f(x: Int, y: Int) -> Bool
              cond
                x > 0 and not x == 50 and y > 0 and not y == 50 or x == 999 or y == 999 or not x == y -> true
                else -> false
              end
            end
        ", "
            fn f(x: Int, y: Int) -> Bool
              cond
                x > 0 and not x == 50 and y > 0 and not y == 50 or x == 999 or y == 999 or
                not x == y ->
                  true

                else ->
                  false
              end
            end
        ");
    }

    #[test]
    fn cond_and_chain_packs_like_fill() {
        assert_fmt("
            fn f(x: Int, y: Int) -> Bool
              cond
                x > 0 and x < 100 and y > 0 and y < 100 and x != y and x != 50 and y != 50 and x != 99 -> true
                else -> false
              end
            end
        ", "
            fn f(x: Int, y: Int) -> Bool
              cond
                x > 0 and x < 100 and y > 0 and y < 100 and x != y and x != 50 and
                y != 50 and x != 99 ->
                  true

                else ->
                  false
              end
            end
        ");
    }

    #[test]
    fn short_closure_inline() {
        assert_fmt(
            "
            fn apply(f: fn(Int) -> Int, x: Int) -> Int
              f(x)
            end

            fn main
              apply(x -> x * 2, 5)
            end
        ",
            "
            fn apply(f: fn (Int) -> Int, x: Int) -> Int
              f(x)
            end

            fn main
              apply(x -> x * 2, 5)
            end
        ",
        );
    }

    #[test]
    fn short_block_closure_assignment_stays_inline() {
        assert_fmt(
            "
            fn main
              f =
                fn (x: Int, y: Int) -> Int x + y end
            end
        ",
            "
            fn main
              f = fn (x: Int, y: Int) -> Int x + y end
            end
        ",
        );
    }

    #[test]
    fn long_block_closure_assignment_breaks_after_eq() {
        assert_fmt(
            "
            fn main
              transform = fn (input_value: Int, scaling_factor: Int) -> Int input_value * scaling_factor + 1 end
            end
        ",
            "
            fn main
              transform =
                fn (input_value: Int, scaling_factor: Int) -> Int
                  input_value * scaling_factor + 1
                end
            end
        ",
        );
    }

    #[test]
    fn binary_literal_formatting() {
        assert_fmt(
            "
            fn main
              b = <<1, 2, 3>>
              c = <<header::8, payload::16 big>>
            end
        ",
            "
            fn main
              b = <<1, 2, 3>>
              c = <<header::8, payload::16 big>>
            end
        ",
        );
    }

    #[test]
    fn concat_operator() {
        assert_fmt(
            r#"
            fn main
              s = "hello" <> " " <> "world"
            end
        "#,
            r#"
            fn main
              s = "hello" <> " " <> "world"
            end
        "#,
        );
    }

    #[test]
    fn struct_construction_short_inline() {
        assert_fmt(
            r#"
            fn main
              c = Config{name: "yo", enabled: true}
            end
        "#,
            r#"
            fn main
              c = Config{name: "yo", enabled: true}
            end
        "#,
        );
    }

    #[test]
    fn struct_construction_long_multiline() {
        assert_fmt(
            r#"
            fn main
              c = Config{name: "a very long name here", enabled: true, verbose: false, timeout: 3000}
            end
        "#,
            r#"
            fn main
              c = Config{
                name: "a very long name here",
                enabled: true,
                verbose: false,
                timeout: 3000,
              }
            end
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
    fn blank_line_before_block_statement() {
        assert_fmt(
            "
            fn f(x: Int) -> Int
              y = x + 1
              if y > 10
                y = y - 10
              end
              y
            end
        ",
            "
            fn f(x: Int) -> Int
              y = x + 1

              if y > 10
                y = y - 10
              end

              y
            end
        ",
        );
    }

    #[test]
    fn no_blank_line_when_block_is_first_or_last() {
        assert_fmt(
            "
            fn f(x: Int) -> Int
              if x > 0
                x
              else
                -x
              end
            end
        ",
            "
            fn f(x: Int) -> Int
              if x > 0
                x
              else
                -x
              end
            end
        ",
        );
    }

    #[test]
    fn blank_line_between_adjacent_blocks() {
        assert_fmt(
            "
            fn f(x: Int)
              if x > 0
                print(x)
              end
              while x > 0
                x -= 1
              end
            end
        ",
            "
            fn f(x: Int)
              if x > 0
                print(x)
              end

              while x > 0
                x -= 1
              end
            end
        ",
        );
    }

    #[test]
    fn method_chain_short_stays_inline() {
        assert_fmt(
            r#"
            fn f -> String
              "hello".upcase().trim()
            end
        "#,
            r#"
            fn f -> String
              "hello".upcase().trim()
            end
        "#,
        );
    }

    #[test]
    fn method_chain_long_breaks_per_call() {
        assert_fmt(
            r#"
            fn build -> String
              sb = StringBuilder.new().add("GET").add(" ").add("/index.html").add(" HTTP/1.1\r\n").add("Host: ").add("example.com").add("\r\n")
              sb.build()
            end
        "#,
            r#"
            fn build -> String
              sb = StringBuilder.new()
                .add("GET")
                .add(" ")
                .add("/index.html")
                .add(" HTTP/1.1\r\n")
                .add("Host: ")
                .add("example.com")
                .add("\r\n")
              sb.build()
            end
        "#,
        );
    }

    #[test]
    fn method_chain_block_gets_spacing() {
        assert_fmt(
            r#"
            fn build(body: String) -> String
              sb = StringBuilder.new().add("GET / HTTP/1.1\r\n").add("Host: example.com\r\n").add("\r\n")
              if not body.empty?()
                sb = sb.add(body)
              end
              sb.build()
            end
        "#,
            r#"
            fn build(body: String) -> String
              sb = StringBuilder.new()
                .add("GET / HTTP/1.1\r\n")
                .add("Host: example.com\r\n")
                .add("\r\n")

              if not body.empty?()
                sb = sb.add(body)
              end

              sb.build()
            end
        "#,
        );
    }

    #[test]
    fn cond_or_chain_packs_like_fill() {
        assert_fmt(
            r#"
            fn f(x: String) -> String
              cond
                x == "alpha" or x == "bravo" or x == "charlie" or x == "delta" or x == "echo" or x == "foxtrot" or x == "golf" -> "nato"
                else -> "other"
              end
            end
        "#,
            r#"
            fn f(x: String) -> String
              cond
                x == "alpha" or x == "bravo" or x == "charlie" or x == "delta" or
                x == "echo" or x == "foxtrot" or x == "golf" ->
                  "nato"

                else ->
                  "other"
              end
            end
        "#,
        );
    }

    #[test]
    fn match_long_single_expr_body_breaks_all_arms() {
        // One arm's single-expression body overflows the page width, so
        // every sibling arm body is pushed onto its own line with blank
        // lines between arms -- not just the long one.
        assert_fmt(
            r#"
            fn pick(x: Option<Int>) -> String
              match x
                Option.Some(value) -> compute_the_chosen_label_for_the_final_display(value)
                Option.None -> "none"
              end
            end
        "#,
            r#"
            fn pick(x: Option<Int>) -> String
              match x
                Option.Some(value) ->
                  compute_the_chosen_label_for_the_final_display(value)

                Option.None ->
                  "none"
              end
            end
        "#,
        );
    }

    #[test]
    fn match_short_bodies_stay_inline() {
        // Regression guard: when no arm body overflows, the whole match
        // stays inline with no blank lines between arms.
        assert_fmt(
            "
            fn f(x: Int) -> Int
              match x
                1 -> 10
                _ -> 0
              end
            end
        ",
            "
            fn f(x: Int) -> Int
              match x
                1 -> 10
                _ -> 0
              end
            end
        ",
        );
    }

    #[test]
    fn match_ternary_body_breaks_arms_consistently() {
        // The `scan_error` shape: a ternary body overflows, so the short
        // sibling arm breaks consistently rather than staying inline.
        assert_fmt(
            r#"
            fn scan_error(field: Int, rest: Binary) -> String
              match take_cstring(rest)
                Option.Some(pair) -> field == 77 ? pair.first : scan_error_more(pair.second)
                Option.None -> "error"
              end
            end
        "#,
            r#"
            fn scan_error(field: Int, rest: Binary) -> String
              match take_cstring(rest)
                Option.Some(pair) ->
                  field == 77 ? pair.first : scan_error_more(pair.second)

                Option.None ->
                  "error"
              end
            end
        "#,
        );
    }
}
