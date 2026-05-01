//! Control-flow constructs: `if`/`else`, ternary `?:`, `loop`,
//! `while`, `cond`, `match` (and future `for`).
//!
//! Maps to LANGUAGE.md "Control Flow".

mod common;

use common::{dedent, eval_entry};
use expo_ir_eval::Value;

#[test]
fn evaluates_if_else() {
    let source = "
        fn sign(x: Int) -> Int
          if x > 0
            1
          else
            -1
          end
        end

        fn run -> Int
          sign(-5) + sign(7)
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(0));
}

#[test]
fn evaluates_if_no_else() {
    // `if`-no-else lowers to a `CondBranch { then: body, otherwise:
    // merge }` plus a body block whose default exit branches to
    // merge. No phi -- the construct is statement-shaped and the
    // operand is `Unit`. Both the taken and skipped paths must
    // converge on the merge with the local mutated / untouched
    // accordingly.
    let source = "
        fn taken -> Int
          x = 0
          if true
            x = 5
          end
          x
        end

        fn skipped -> Int
          x = 0
          if false
            x = 5
          end
          x
        end

        fn run -> Int
          taken() + skipped()
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(5));
}

#[test]
fn evaluates_unless() {
    // `unless` is the polarity twin of `if`-no-else: the body lands
    // on the `otherwise` slot of the entry `CondBranch`. Skipped
    // when the condition is true, taken when false.
    let source = "
        fn skipped -> Int
          x = 0
          unless true
            x = 5
          end
          x
        end

        fn taken -> Int
          x = 0
          unless false
            x = 5
          end
          x
        end

        fn run -> Int
          taken() + skipped()
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(5));
}

#[test]
fn evaluates_cond_with_else() {
    // `cond` chains per-arm check blocks: each
    // `arm[i].check.otherwise` branches to `arm[i+1].check`, the
    // last arm's `otherwise` lands on the else block, and the else
    // body's exit branches to the merge. The parser requires an
    // `else ->` arm (no-else `cond` is only an error-recovery AST
    // shape), so this exercises the reachable shape.
    let source = "
        fn matches_second -> Int
          a = 0
          b = 0
          c = 0
          cond
            1 == 2 -> a = 10
            2 == 2 -> b = 20
            3 == 4 -> c = 30
            else -> a = -1
          end
          a + b + c
        end

        fn matches_else -> Int
          a = 0
          b = 0
          cond
            1 == 2 -> a = 10
            3 == 4 -> a = 20
            else -> b = 7
          end
          a + b
        end

        fn run -> Int
          matches_second() + matches_else()
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(27));
}

#[test]
fn evaluates_while() {
    // `while` lowers to a `while_header` + body + exit CFG. The
    // back-edge from body -> header rewrites `i` in
    // `Frame::locals`, so a fresh `Load` of `i` on the next header
    // re-evaluation sees the mutated value. Confirms the lifted
    // arm threads through the same locals scope across iterations.
    let source = "
        fn run -> Int
          i = 0
          while i < 5
            i = i + 1
          end
          i
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(5));
}

#[test]
fn evaluates_while_zero_iterations() {
    // Header condition is false on entry, so the body block never
    // runs and `i` is observed unchanged after the loop.
    let source = "
        fn run -> Int
          i = 7
          while i > 100
            i = 0
          end
          i
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(7));
}

#[test]
fn evaluates_loop_with_break() {
    // `loop` produces an unconditional back-edge with no header;
    // exit happens only via `Statement::Break` which terminates the
    // current block with `Branch(loop_exit)`. The if/else arms are
    // both fully lifted (one increments + falls through to the
    // back-edge, the other breaks), so no Stub survives to
    // interpretation.
    let source = "
        fn run -> Int
          i = 0
          loop
            if i < 3
              i = i + 1
            else
              break
            end
          end
          i
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(3));
}

#[test]
fn evaluates_nested_loops_with_inner_break() {
    // Regression: pre-cleanup, the `Loop` arm in
    // `lower_expr_to_operand_with_tail` over-popped `loop_exit` after
    // calling `lower_loop`. With nesting, the inner pop corrupted the
    // outer's exit slot mid-body lowering and any direct break in the
    // outer body's continuation would IR-fail with "break outside of
    // loop". This exercises that path: outer body lowers an inner
    // loop (which pushes/pops inner_exit internally), then a direct
    // `break` in the outer's continuation must still resolve to
    // outer_exit.
    let source = "
        fn run -> Int
          i = 0
          loop
            j = 0
            loop
              if j < 2
                j = j + 1
              else
                break
              end
            end
            i = i + j

            if i < 6
              i = i
            else
              break
            end
          end
          i
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(6));
}

#[test]
fn evaluates_ternary() {
    // Ternary lowers via `Lowerer::lower_ternary` to a `CondBranch` +
    // arm blocks + `Phi` -- the same merge shape as `if`/`else` --
    // so this test confirms both arms route correctly through the
    // interpreter's `Phi` handler when called via the ternary syntax.
    let source = "
        fn sign(x: Int) -> Int
          x > 0 ? 1 : -1
        end

        fn run -> Int
          sign(-5) + sign(7)
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(0));
}

#[test]
fn evaluates_match_literal_arms() {
    // Literal arms with `_` catch-all.
    let source = "
        fn classify(x: Int) -> Int
          match x
            1 -> 100
            2 -> 200
            3 -> 300
            _ -> 999
          end
        end

        fn run -> Int
          classify(1) + classify(2) + classify(3) + classify(7)
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(1599));
}

#[test]
fn evaluates_match_binding() {
    // Bare binding always matches and exposes the subject as a local.
    let source = "
        fn run -> Int
          x = 5
          match x
            v -> v + 100
          end
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(105));
}

#[test]
fn evaluates_match_wildcard_only() {
    // Single `_ ->` arm: exhaustive trivially.
    let source = "
        fn run -> Int
          x = 17
          match x
            _ -> 42
          end
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(42));
}

#[test]
fn evaluates_match_struct_destructure() {
    // Plain struct patterns: literal-test + binding mix, then
    // binding-only fallback.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn classify(p: Point) -> Int
          match p
            Point{x: 5, y: y} -> y
            Point{x: x, y: _} -> x
          end
        end

        fn run -> Int
          classify(Point{x: 5, y: 17}) + classify(Point{x: 9, y: 100})
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(26));
}

#[test]
fn evaluates_match_struct_partial() {
    // Partial struct patterns: unlisted fields are implicit wildcards.
    let source = "
        struct Triple
          a: Int
          b: Int
          c: Int
        end

        fn route(t: Triple) -> Int
          match t
            Triple{a: 0} -> 999
            Triple{a: a, c: c} -> a * 100 + c
          end
        end

        fn run -> Int
          route(Triple{a: 0, b: 1, c: 2}) + route(Triple{a: 3, b: 4, c: 5})
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(1304));
}
