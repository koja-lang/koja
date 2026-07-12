pub mod doc;
pub mod printer;

use doc::render;
use koja_ast::ast::Diagnostic;
use koja_parser::ParseMode;

/// The result of formatting a source string.
pub enum FormatResult {
    /// Successfully formatted source code.
    Ok(String),
    /// The source could not be parsed. Carries the parse diagnostics.
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
        fmt_mode(source, ParseMode::File)
    }

    fn fmt_script(source: &str) -> String {
        fmt_mode(source, ParseMode::Script)
    }

    fn fmt_mode(source: &str, mode: ParseMode) -> String {
        match format(&dedent(source), mode) {
            FormatResult::Ok(s) => s,
            FormatResult::ParseErrors(e) => panic!("parse error: {:?}", e),
        }
    }

    fn assert_fmt(input: &str, expected: &str) {
        assert_formatted(fmt(input), expected);
    }

    fn assert_fmt_script(input: &str, expected: &str) {
        assert_formatted(fmt_script(input), expected);
    }

    /// Assert `source` is already in canonical form (formatting it
    /// changes nothing).
    fn assert_unchanged(source: &str) {
        assert_fmt(source, source);
    }

    fn assert_formatted(actual: String, expected: &str) {
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
                x > 0 and not x == 50 and y > 0 and not y == 50 or x == 999 or y == 999
                  or not x == y ->
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
                x > 0 and x < 100 and y > 0 and y < 100 and x != y and x != 50 and y != 50
                  and x != 99 ->
                  true

                else ->
                  false
              end
            end
        ");
    }

    #[test]
    fn short_closure_inline() {
        assert_fmt_script(
            "
            fn apply(f: fn(Int) -> Int, x: Int) -> Int
              f(x)
            end

            apply(x -> x * 2, 5)
        ",
            "
            fn apply(f: fn (Int) -> Int, x: Int) -> Int
              f(x)
            end

            apply(x -> x * 2, 5)
        ",
        );
    }

    #[test]
    fn short_block_closure_assignment_stays_inline() {
        assert_fmt_script(
            "
            f =
              fn (x: Int, y: Int) -> Int x + y end
        ",
            "
            f = fn (x: Int, y: Int) -> Int x + y end
        ",
        );
    }

    #[test]
    fn long_block_closure_assignment_breaks_after_eq() {
        assert_fmt_script(
            "
            transform = fn (input_value: Int, scaling_factor: Int) -> Int input_value * scaling_factor + 1 end
        ",
            "
            transform =
              fn (input_value: Int, scaling_factor: Int) -> Int
                input_value * scaling_factor + 1
              end
        ",
        );
    }

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
    fn wrapped_condition_indents_and_separates_from_body() {
        // A condition too long for one line mirrors wrapped function
        // heads: continuation indented two past the keyword, blank
        // line before the body.
        assert_unchanged(
            "
            fn f(alpha: Bool, bravo: Bool, charlie: Bool, delta: Bool) -> Int
              if alpha and bravo and charlie and delta and alpha and bravo and charlie
                and delta

                1
              else
                2
              end
            end
        ",
        );
    }

    #[test]
    fn wrapped_while_condition_indents_and_separates_from_body() {
        assert_unchanged(
            "
            fn f(first_operand: Bool, second_operand: Bool, third_operand: Bool) -> Int
              while first_operand and second_operand and third_operand and first_operand
                and second_operand

                1
              end

              2
            end
        ",
        );
    }

    #[test]
    fn wrapped_chain_in_statement_position_hangs_two() {
        // A chain that wraps outside a condition header still indents
        // its continuation lines two past the statement start.
        assert_unchanged(
            r#"
            fn f(text: String) -> Bool
              text.contains?("-----BEGIN PRIVATE KEY-----")
                or text.contains?("-----BEGIN RSA PRIVATE KEY-----")
                or text.contains?("-----BEGIN EC PRIVATE KEY-----")
            end
        "#,
        );
    }

    #[test]
    fn short_condition_stays_inline_without_blank_line() {
        assert_unchanged(
            "
            fn f(x: Int) -> Int
              if x > 10
                1
              else
                2
              end
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
              fn take_u16(data: Binary) -> Option<Pair<Int, Binary>>
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
        // Matches the struct/enum trailing behavior: the comment attaches
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
    fn struct_literal_field_trailing_comment_survives() {
        assert_unchanged(
            "
            fn f -> Point
              Point{
                x: 1, # horizontal
                y: 2,
              }
            end
        ",
        );
    }

    #[test]
    fn enum_struct_literal_field_trailing_comment_survives() {
        assert_unchanged(
            "
            fn f -> Shape
              Shape.Rect{
                width: 1, # px
                height: 2,
              }
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
                x == "alpha" or x == "bravo" or x == "charlie" or x == "delta"
                  or x == "echo" or x == "foxtrot" or x == "golf" ->
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
        // every sibling arm body (not just the long one) is pushed onto
        // its own line with blank lines between arms.
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
    fn depth_two_chain_breaks_at_dots() {
        // A call-rooted depth-2 chain that overflows breaks at every dot
        // (the root call does not glue the first `.method`), and each sole
        // closure argument is hugged rather than exploded.
        assert_fmt(
            r#"
            fn run_user(args: List<String>) -> Result<String, String>
              require_arg(args, "<login>").then(login -> GitHub.user(login)).map(x -> render_user(x))
            end
        "#,
            r#"
            fn run_user(args: List<String>) -> Result<String, String>
              require_arg(args, "<login>")
                .then(login -> GitHub.user(login))
                .map(x -> render_user(x))
            end
        "#,
        );
    }

    #[test]
    fn sole_short_closure_arg_does_not_explode() {
        // An overflowing single call with a sole short-closure argument stays
        // hugged on one line rather than exploding the arg list.
        assert_fmt(
            r#"
            fn f(opt: Option<Int>) -> Option<Int>
              some_extremely_long_receiver_variable_name_here.map(value -> value_plus_something)
            end
        "#,
            r#"
            fn f(opt: Option<Int>) -> Option<Int>
              some_extremely_long_receiver_variable_name_here.map(value -> value_plus_something)
            end
        "#,
        );
    }

    #[test]
    fn sole_block_closure_arg_hugs_and_breaks_internally() {
        // A sole block-closure argument hugs the parens and breaks inside the
        // closure body, with `end)` closing both the closure and the call.
        assert_fmt(
            r#"
            fn f(nums: List<Int>) -> List<Int>
              nums.map(fn (n: Int) -> Int compute_a_doubled_display_value_for_each_number(n) end)
            end
        "#,
            r#"
            fn f(nums: List<Int>) -> List<Int>
              nums.map(fn (n: Int) -> Int
                compute_a_doubled_display_value_for_each_number(n)
              end)
            end
        "#,
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
