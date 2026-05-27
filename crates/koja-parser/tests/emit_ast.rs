//! End-to-end snapshot tests for [`koja_ast::format_file`], the
//! compact tree printer used by `koja parse --emit-ast` and `koja
//! check --emit-ast`. These tests exercise the printer through real
//! parses so span math, operator precedence, and structural layout
//! all match what a user sees from the CLI.

use std::path::PathBuf;

use koja_parser::{ParseMode, SourceFile, parse_file};

fn format_snippet(source: &str) -> String {
    let parsed = parse_file(
        SourceFile {
            package: "TestApp".to_string(),
            path: PathBuf::from("/tmp/demo.koja"),
            source: source.to_string(),
        },
        ParseMode::File,
    );
    assert!(
        parsed.diagnostics.is_empty(),
        "parse errors: {:#?}",
        parsed.diagnostics
    );
    koja_ast::format_file(&parsed.ast)
}

fn format_snippet_script(source: &str) -> String {
    let parsed = parse_file(
        SourceFile {
            package: "TestApp".to_string(),
            path: PathBuf::from("/tmp/demo.koja"),
            source: source.to_string(),
        },
        ParseMode::Script,
    );
    assert!(
        parsed.diagnostics.is_empty(),
        "parse errors: {:#?}",
        parsed.diagnostics
    );
    koja_ast::format_file(&parsed.ast)
}

#[test]
fn addition_literal() {
    let out = format_snippet_script("2 + 2\n");
    let expected = r#"File TestApp "/tmp/demo.koja" @1:1-2:1
  body
    Binary Add @1:1-1:6
      Literal Int 2 @1:1-1:2
      Literal Int 2 @1:5-1:6
"#;
    assert_eq!(out, expected, "got:\n{out}");
}

#[test]
fn function_add() {
    let source = "fn add(a: Int, b: Int) -> Int\n  a + b\nend\n";
    let out = format_snippet(source);
    let expected = r#"File TestApp "/tmp/demo.koja" @1:1-4:1
  items
    Function add (Public) @1:1-3:4
      params
        Regular a: Named Int (Borrow) @1:8-1:14
        Regular b: Named Int (Borrow) @1:16-1:22
      return: Named Int
      body
        Binary Add @2:3-2:8
          Ident a @2:3-2:4
          Ident b @2:7-2:8
"#;
    assert_eq!(out, expected, "got:\n{out}");
}

#[test]
fn multi_feature_file() {
    let source = "\
struct Point
  x: Int
  y: Int
end

enum Shape
  Circle
  Rect(Int, Int)
end

fn describe(p: Point) -> Int
  result = p.x + p.y
  cond
    result == 0 ->
      return 0
    else ->
      return result
  end
end
";
    let out = format_snippet(source);
    // Assertion strategy: check stable structural anchors rather than
    // the entire blob. Exact span numbers are validated by the simpler
    // tests above; this one verifies the shape of each item, the
    // nested `cond`, pattern matching, etc.
    let must_contain = [
        "File TestApp \"/tmp/demo.koja\"",
        "StructDecl Point",
        "  fields",
        "    x: Named Int",
        "    y: Named Int",
        "EnumDecl Shape",
        "  variants",
        "    EnumVariant Circle (Unit)",
        "    EnumVariant Rect (Tuple)",
        "      type: Named Int",
        "Function describe (Public)",
        "      params",
        "        Regular p: Named Point (Borrow)",
        "      return: Named Int",
        "      body",
        "        Assignment",
        "          target: result",
        "          value",
        "            Binary Add",
        "              FieldAccess .x",
        "                Ident p",
        "              FieldAccess .y",
        "                Ident p",
        "        Cond",
        "          arm",
        "            condition",
        "              Binary Eq",
        "                Ident result",
        "                Literal Int 0",
        "            body",
        "              Return",
        "                Literal Int 0",
        "          else",
        "            Return",
        "              Ident result",
    ];
    for needle in must_contain {
        assert!(out.contains(needle), "missing `{needle}` in output:\n{out}");
    }
}
