//! IR-text coverage for nested type names: a nested struct emits a
//! distinctly-mangled LLVM type and its `extend` method emits a
//! distinctly-mangled symbol, so nesting never collides with a
//! top-level type of the same leaf name. A type that implements two
//! nested protocols with the same leaf name (`Date.Format`,
//! `Clock.Format`) emits both impl methods.

use koja_ast::util::dedent;
use koja_ir_llvm::emit_script_llvm_ir;

mod common;

use common::{APP_NAME, assert_contains, lower_script_source as lower};

#[test]
fn nested_struct_and_method_mangle_with_full_path() {
    let source = "
        struct Outer
          tag: Int
        end

        struct Outer.Inner
          x: Int
        end

        extend Outer.Inner
          fn doubled(self) -> Int
            self.x * 2
          end
        end

        Outer.Inner{x: 21}.doubled()
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(&ir_text, "%TestApp.Outer.Inner = type { i64 }");
    assert_contains(&ir_text, "@\"TestApp.Outer.Inner.doubled/1\"(");
}

#[test]
fn nested_protocol_impls_emit_one_method_per_protocol() {
    let source = "
        struct Date
          day: Int

          protocol Format
            fn format_date(self, date: Date) -> String
          end
        end

        struct Clock
          hour: Int
        end

        protocol Clock.Format
          fn format_clock(self, clock: Clock) -> String
        end

        enum ISO8601
          Basic
        end

        impl Date.Format for ISO8601
          fn format_date(self, date: Date) -> String
            \"day #{date.day}\"
          end
        end

        impl Clock.Format for ISO8601
          fn format_clock(self, clock: Clock) -> String
            \"hour #{clock.hour}\"
          end
        end

        fn render<F: Date.Format>(date: Date, formatter: F) -> String
          formatter.format_date(date)
        end

        \"#{render(Date{day: 3}, ISO8601.Basic)} #{ISO8601.Basic.format_clock(Clock{hour: 7})}\"
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(&ir_text, "@\"TestApp.ISO8601.format_date/2\"(");
    assert_contains(&ir_text, "@\"TestApp.ISO8601.format_clock/2\"(");
}
