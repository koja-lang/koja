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

    // The comment attachment pass locates boundary keywords (`else`,
    // `after`) in the token stream because the AST carries no spans for
    // them.
    let lexed = koja_lexer::lex(source, koja_ast::span::FileId::UNKNOWN);
    let doc = printer::file_to_doc(&result.ast, &lexed.tokens);
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

    fn assert_unchanged_script(source: &str) {
        assert_fmt_script(source, source);
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
    fn closure_body_trailing_comment_forces_broken_layout() {
        assert_fmt_script(
            "
            add = fn (a: Int32) -> Int32
              a # body comment
            end
        ",
            "
            add =
              fn (a: Int32) -> Int32
                a # body comment
              end
        ",
        );
        assert_unchanged_script(
            "
            add =
              fn (a: Int32) -> Int32
                a # body comment
              end
        ",
        );
    }

    #[test]
    fn closure_body_leading_comment_forces_broken_layout() {
        assert_unchanged_script(
            "
            g =
              fn (x: Int32) -> Int32
                # double it
                x * 2
              end
        ",
        );
    }

    #[test]
    fn closure_end_line_comment_stays_inline() {
        assert_unchanged_script("f = fn () -> Int 42 end # note");
    }

    #[test]
    fn list_comments_stay_with_elements() {
        assert_fmt_script(
            "
            list = [
              # first element
              1,
              2, # trailing
              3,
            ]
        ",
            "
            list = [
              # first element
              1, 2, # trailing
              3
            ]
        ",
        );
    }

    #[test]
    fn list_packing_resumes_around_comments() {
        assert_unchanged_script(
            "
            big = [
              1, 2, # two
              3, 4, 5, 6,
              # header values start here
              7, 8
            ]
        ",
        );
    }

    #[test]
    fn list_comment_before_closing_bracket_stays_inside() {
        assert_unchanged_script(
            "
            closed = [
              1, 2
              # before the bracket
            ]
        ",
        );
    }

    #[test]
    fn map_entry_trailing_comment_stays_with_entry() {
        assert_unchanged_script(
            "
            m = [
              \"a\": 1, # first
              \"b\": 2
            ]
        ",
        );
    }

    #[test]
    fn binary_literal_comment_breaks_its_line() {
        assert_unchanged_script(
            "
            b = <<
              1, 2, # second
              3
            >>
        ",
        );
    }

    #[test]
    fn construction_field_comments_stay_with_fields() {
        assert_unchanged_script(
            "
            p = Point{
              # horizontal
              x: 1,
              y: 2, # vertical
            }
        ",
        );
    }

    #[test]
    fn call_arg_comments_stay_with_args() {
        assert_unchanged_script(
            "
            r = compute(
              # first arg
              2,
              3, # second
            )
        ",
        );
    }

    #[test]
    fn param_comments_force_broken_signature() {
        assert_unchanged(
            "
            fn compute(
              # the base value
              base: Int32,
              scale: Int32, # multiplier
            ) -> Int32

              base * scale
            end
        ",
        );
    }

    #[test]
    fn chain_comment_anchors_to_its_link() {
        assert_unchanged_script(
            "
            out = [3, 1, 2]
              .map(v -> v * 2)
              # drop the small ones
              .filter(v -> v > 2)
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
    fn match_arm_trailing_comment_stays_on_arm() {
        assert_unchanged(
            "
            fn f(x: Int) -> Int
              match x
                1 -> 10 # one
                _ -> 0
              end
            end
        ",
        );
    }

    #[test]
    fn leading_comment_between_match_arms_stays_above_arm() {
        assert_unchanged(
            "
            fn f(x: Int) -> Int
              match x
                1 -> 10
                # everything else
                _ -> 0
              end
            end
        ",
        );
    }

    #[test]
    fn cond_arm_trailing_comment_stays_on_arm() {
        assert_unchanged(
            "
            fn f(x: Int) -> Int
              cond
                x > 10 -> 1 # big
                else -> 0
              end
            end
        ",
        );
    }

    #[test]
    fn receive_arm_trailing_comment_stays_on_arm() {
        assert_unchanged(
            "
            fn f -> Int
              receive
                n: Int -> n # next job
              end
            end
        ",
        );
    }

    #[test]
    fn arm_head_trailing_comment_forces_broken_body() {
        assert_unchanged(
            "
            fn f(x: Int) -> Int
              match x
                1 -> # one
                  10
                _ -> 0
              end
            end
        ",
        );
    }

    #[test]
    fn or_pattern_comment_hoists_above_match_arm() {
        assert_fmt(
            "
            fn f(x: Int) -> Int
              match x
                1 |
                # grouped cases
                2 -> # selected
                  10
                _ -> 0
              end
            end
        ",
            "
            fn f(x: Int) -> Int
              match x
                # grouped cases
                1 | 2 -> # selected
                  10
                _ -> 0
              end
            end
        ",
        );
    }

    #[test]
    fn wrapped_cond_head_comment_hoists_above_arm() {
        assert_fmt(
            "
            fn f(x: Int) -> Int
              cond
                x > 0
                # bounded positive
                and x < 100 -> # selected
                  1
                else -> 0
              end
            end
        ",
            "
            fn f(x: Int) -> Int
              cond
                # bounded positive
                x > 0 and x < 100 -> # selected
                  1
                else -> 0
              end
            end
        ",
        );
    }

    #[test]
    fn wrapped_receive_guard_comment_hoists_above_arm() {
        assert_fmt(
            "
            fn f -> Int
              receive
                n: Int when n > 0
                # bounded positive
                and n < 100 -> # selected
                  n
              end
            end
        ",
            "
            fn f -> Int
              receive
                # bounded positive
                n: Int when n > 0 and n < 100 -> # selected
                  n
              end
            end
        ",
        );
    }

    #[test]
    fn arm_body_leading_comment_forces_broken_body() {
        assert_unchanged(
            "
            fn f(x: Int) -> Bool
              match x
                1 -> true # awesome
                _ ->
                  # kinda meh
                  false
              end
            end
        ",
        );
    }

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

    #[test]
    fn wrapped_cond_arm_head_is_stable() {
        assert_unchanged_script(
            "
            v =
              cond
                no ->
                  \"first\"

                yes and yes and yes and yes and yes and yes and yes and yes and yes and yes
                  and yes and yes ->
                  \"wrapped\"

                else ->
                  \"other\"
              end
        ",
        );
    }

    #[test]
    fn wrapped_match_guard_is_stable() {
        assert_unchanged_script(
            "
            r =
              match n
                0 -> \"zero\"
                x when x > 0 and x < 100 and x != 13 and x != 42 and x != 99 and x != 7
                  and x != 3 ->
                  \"ok\"
                _ -> \"other\"
              end
        ",
        );
    }

    #[test]
    fn collapsed_arm_keeps_body_trailing_comment() {
        assert_fmt(
            "
            fn f(x: Int) -> Int
              match x
                1 ->
                  10 # one
                _ -> 0
              end
            end
        ",
            "
            fn f(x: Int) -> Int
              match x
                1 -> 10 # one
                _ -> 0
              end
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
    fn else_body_leading_comment_stays_in_else() {
        assert_unchanged(
            r#"
            fn f(n: Int32)
              if n > 2
                "big".print()
              else
                # leading comment in else body
                "small".print()
              end
            end
        "#,
        );
    }

    #[test]
    fn comment_before_else_stays_above_else() {
        assert_unchanged(
            r#"
            fn f(n: Int32)
              if n > 2
                "big".print()
              # before else
              else # on else line
                "small".print()
              end
            end
        "#,
        );
    }

    #[test]
    fn cond_comment_before_else_stays_above_else() {
        assert_unchanged(
            r#"
            fn f(n: Int32) -> String
              cond
                n > 2 -> "big"
                # before else
                else -> "small"
              end
            end
        "#,
        );
    }

    #[test]
    fn comment_before_match_end_stays_inside() {
        assert_unchanged(
            r#"
            fn f -> String
              r =
                match 1
                  _ -> "x"
                  # inside comment
                end

              r
            end
        "#,
        );
    }

    #[test]
    fn receive_after_boundary_comments_stay_in_place() {
        assert_unchanged(
            r#"
            fn f
              receive
                msg: Int -> msg.print()
              # before after
              after 10 # timeout ms
                "x".print()
              end
            end
        "#,
        );
    }

    #[test]
    fn chain_statement_leading_comment_stays_above_statement() {
        assert_unchanged(
            r#"
            fn f(code: Int32) -> String
              match code
                1 ->
                  # wrap and copy, never free
                  error_string(code).to_cstring().to_string().unwrap()
                _ -> "other"
              end
            end
        "#,
        );
    }

    #[test]
    fn broken_assignment_head_comment_hoists_above_statement() {
        assert_fmt(
            r#"
            fn f -> String
              r = # note
                match 1
                  _ -> "x"
                end
              r
            end
        "#,
            r#"
            fn f -> String
              # note
              r =
                match 1
                  _ -> "x"
                end

              r
            end
        "#,
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
    fn single_continuation_call_hugs_broken_args() {
        // A chain with one continuation call breaks the anchor's argument
        // list and glues the trailing call to the closing paren instead of
        // dropping it onto its own line.
        assert_fmt(
            r#"
            fn f(settings: Config) -> DbConfig
              DbConfig.new(settings.db_host, settings.db_port, settings.db_user, settings.db_name).with_password(settings.db_password)
            end
        "#,
            r#"
            fn f(settings: Config) -> DbConfig
              DbConfig.new(
                settings.db_host,
                settings.db_port,
                settings.db_user,
                settings.db_name,
              ).with_password(settings.db_password)
            end
        "#,
        );
    }

    #[test]
    fn single_continuation_call_breaks_at_dot_when_anchor_fits() {
        // When the anchor fits inline but the trailing call overflows, the
        // chain still breaks at the dot.
        assert_fmt(
            r#"
            fn f(settings: Config) -> DbConfig
              DbConfig.new(settings.db_host, settings.db_port).with_password(settings.extremely_long_database_password_field)
            end
        "#,
            r#"
            fn f(settings: Config) -> DbConfig
              DbConfig.new(settings.db_host, settings.db_port)
                .with_password(settings.extremely_long_database_password_field)
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
              require_arg(args, "<login>").map(login -> GitHub.user(login)).map_err(e -> render_error(e))
            end
        "#,
            r#"
            fn run_user(args: List<String>) -> Result<String, String>
              require_arg(args, "<login>")
                .map(login -> GitHub.user(login))
                .map_err(e -> render_error(e))
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

    /// Cooked value of the first string expression in dedented script
    /// `source`, with interpolations rendered as `#{...}` placeholders.
    fn first_string_value(source: &str) -> String {
        use koja_ast::ast::{Statement, StringPart};
        let result = koja_parser::parse(&dedent(source), ParseMode::Script);
        assert!(
            result.errors.is_empty(),
            "parse errors: {:?}",
            result.errors
        );
        let expr = result
            .ast
            .body
            .unwrap_or_default()
            .into_iter()
            .find_map(|stmt| match stmt {
                Statement::Assignment { value, .. } | Statement::Expr(value) => Some(value),
                _ => None,
            })
            .expect("no expression in script");
        match expr.kind {
            koja_ast::ast::ExprKind::String { parts, .. } => parts
                .iter()
                .map(|p| match p {
                    StringPart::Literal { value, .. } => value.clone(),
                    StringPart::Interpolation { .. } => "#{...}".to_string(),
                })
                .collect(),
            other => panic!("expected string expression, got {other:?}"),
        }
    }

    #[test]
    fn heredoc_broken_opener_round_trips() {
        assert_unchanged(
            r#"
            fn scaffold -> String
              content =
                """
                line one
                  nested deeper
                line two
                """
              content
            end
        "#,
        );
    }

    #[test]
    fn heredoc_glued_opener_round_trips() {
        assert_unchanged(
            r#"
            fn scaffold -> String
              content = """
              line one
              line two
              """
              content
            end
        "#,
        );
    }

    #[test]
    fn heredoc_glued_normalizes_content_to_ambient_indent() {
        let input = r#"
            x = """
              hello
                nested
              """
        "#;
        let output = fmt_script(input);
        assert_formatted(
            output.clone(),
            r#"
            x = """
            hello
              nested
            """
        "#,
        );
        // The cooked value survives the re-indent byte-exactly.
        assert_eq!(first_string_value(input), "hello\n  nested");
        assert_eq!(first_string_value(&output), "hello\n  nested");
    }

    #[test]
    fn heredoc_blank_lines_and_trailing_newline_round_trip() {
        let source = r#"
            x = """
            hello

            world

            """
        "#;
        assert_fmt_script(source, source);
        assert_eq!(first_string_value(source), "hello\n\nworld\n");
    }

    #[test]
    fn heredoc_interpolation_round_trips() {
        assert_unchanged(
            r#"
            fn greet(name: String) -> String
              """
              hello #{name}
              bye
              """
            end
        "#,
        );
    }

    #[test]
    fn heredoc_quotes_round_trip() {
        // Lone quotes stay raw, even at line end. A would-be closing
        // run keeps its escaped third quote.
        assert_unchanged(
            r#"
            fn f -> String
              """
              say "hi"
              a quoted run: ""\"
              """
            end
        "#,
        );
    }

    #[test]
    fn heredoc_sole_call_argument_hugs() {
        assert_unchanged(
            r#"
            fn f
              IO.puts("""
              hello
              """)
            end
        "#,
        );

        // An exploded sole-heredoc argument list collapses to the hug.
        assert_fmt(
            r#"
            fn f
              IO.puts(
                """
                hello
                """,
              )
            end
        "#,
            r#"
            fn f
              IO.puts("""
              hello
              """)
            end
        "#,
        );
    }

    #[test]
    fn heredoc_trailing_space_falls_back_to_single_line() {
        // The renderer trims line ends, so trailing spaces cannot survive
        // heredoc form. Both mid-content and content-final trailing
        // spaces round-trip through the single-line spelling.
        let mid = "x = \"\"\"\nkeep \ntrailing\n\"\"\"\n";
        match format(mid, ParseMode::Script) {
            FormatResult::Ok(out) => assert_eq!(out, "x = \"keep \\ntrailing\"\n"),
            FormatResult::ParseErrors(e) => panic!("parse error: {e:?}"),
        }

        let last = "x = \"\"\"\nkeep \n\"\"\"\n";
        match format(last, ParseMode::Script) {
            FormatResult::Ok(out) => assert_eq!(out, "x = \"keep \"\n"),
            FormatResult::ParseErrors(e) => panic!("parse error: {e:?}"),
        }
    }

    #[test]
    fn error_channel_signature_stays_inline() {
        assert_unchanged(
            "
            fn parse(s: String) -> Int ! ParseError
              1
            end
        ",
        );
    }

    #[test]
    fn union_error_signature_stays_inline() {
        assert_unchanged(
            "
            fn fetch(url: String) -> Limits ! HTTP.Error | ParseError
              parse(url)
            end
        ",
        );
    }

    #[test]
    fn protocol_method_error_signature_round_trips() {
        assert_unchanged(
            "
            protocol Decode
              fn decode(self, raw: Binary) -> Self ! DecodeError
            end
        ",
        );
    }

    #[test]
    fn bare_error_signature_round_trips() {
        assert_unchanged(
            "
            fn ping(host: String) ! String
              send(host)
            end

            protocol Store
              fn flush(self) ! StoreError
            end
        ",
        );
    }

    #[test]
    fn unit_error_signature_normalizes_to_bare_form() {
        assert_fmt(
            "
            fn ping(host: String) -> () ! String
              send(host)
            end
        ",
            "
            fn ping(host: String) ! String
              send(host)
            end
        ",
        );
    }

    #[test]
    fn try_and_fail_round_trip() {
        assert_unchanged(
            "
            fn caller(flag: Bool) -> Int ! MyError
              unless flag
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

    #[test]
    fn short_call_with_inline_closure_stays_glued() {
        // The assignment only breaks when the value renders as a
        // multi-line block. An inline-fitting closure does not count.
        assert_unchanged_script("ref = Task.async(fn () -> Int 42 end)");
    }

    #[test]
    fn call_with_multiline_closure_breaks_after_equals() {
        assert_fmt_script(
            "
            ref = Task.async(fn () -> Int
              a = 1
              a + 1
            end)
        ",
            "
            ref =
              Task.async(fn () -> Int
                a = 1
                a + 1
              end)
        ",
        );
    }

    #[test]
    fn closure_with_block_body_takes_broken_layout() {
        // A single-statement body that is itself a block never
        // collapses onto the signature line.
        assert_unchanged_script(
            r#"
            g =
              fn () -> String
                match 1
                  1 -> "a"
                  _ -> "b"
                end
              end
        "#,
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

    #[test]
    fn protocol_method_signature_wraps_at_width() {
        // Protocol method signatures share the function signature
        // renderer and wrap the same way.
        assert_fmt(
            "
            protocol Transport
              fn send_datagram_with_options(self, payload: Binary, destination_address: String, destination_port: Int32, ttl: Int32) -> Int ! SendError
            end
        ",
            "
            protocol Transport
              fn send_datagram_with_options(
                self,
                payload: Binary,
                destination_address: String,
                destination_port: Int32,
                ttl: Int32,
              ) -> Int ! SendError
            end
        ",
        );
    }

    #[test]
    fn long_union_alias_packs_with_trailing_pipe() {
        // Unions pack like symbolic operator chains.
        assert_fmt(
            "
            type IncomingNetworkEvent = ConnectionEstablished | ConnectionClosed | DataReceived | HandshakeTimeout | ProtocolViolation
        ",
            "
            type IncomingNetworkEvent = ConnectionEstablished | ConnectionClosed |
              DataReceived | HandshakeTimeout | ProtocolViolation
        ",
        );
    }

    #[test]
    fn generic_params_with_bounds_wrap_like_parens() {
        // The angle-bracket list breaks one entry per line, and an
        // entry keeps its bounds intact.
        assert_fmt(
            "
            fn merge_sorted<TElement: Comparable & Hash & Equality, TCollection: Iterable & Equality>(left: TCollection, right: TCollection) -> TCollection
              left
            end
        ",
            "
            fn merge_sorted<
              TElement: Comparable & Hash & Equality,
              TCollection: Iterable & Equality
            >(left: TCollection, right: TCollection) -> TCollection

              left
            end
        ",
        );
    }

    #[test]
    fn return_tail_wraps_like_a_ternary() {
        // A wrapped `-> T ! E` tail puts each segment on its own
        // continuation line starting with its operator, like the
        // branches of a broken ternary.
        assert_fmt(
            "
            fn parse_configuration_file(path: String) -> ConfigurationDocumentWithExtendedMetadata ! ConfigurationParseOrValidationError
              1
            end
        ",
            "
            fn parse_configuration_file(path: String)
              -> ConfigurationDocumentWithExtendedMetadata
              ! ConfigurationParseOrValidationError

              1
            end
        ",
        );
    }

    #[test]
    fn call_on_list_literal_breaks_the_literal_first() {
        // The literal's brackets split before the argument list,
        // and the call hugs the closing bracket.
        assert_fmt_script(
            r#"
            names = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"].map(name -> name.length())
        "#,
            r#"
            names = [
              "alpha", "bravo", "charlie", "delta", "echo", "foxtrot"
            ].map(name -> name.length())
        "#,
        );
    }

    #[test]
    fn call_on_map_literal_breaks_the_literal_first() {
        assert_fmt_script(
            r#"
            lookup = ["alpha": 1, "bravo": 2, "charlie": 3, "delta": 4, "echo": 5].get("delta")
        "#,
            r#"
            lookup = [
              "alpha": 1, "bravo": 2, "charlie": 3, "delta": 4, "echo": 5
            ].get("delta")
        "#,
        );
    }
}
