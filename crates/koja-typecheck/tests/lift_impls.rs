//! Typecheck coverage for the impl-block side of `lift_signatures`:
//! happy-path conformance, default-method override / synthesis,
//! conformance diagnostics (missing / extra / sig mismatches), and
//! the inherent-vs-trait collision case.

use koja_ast::ast::{Item, Visibility};
use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_typecheck::GlobalKind;

mod common;

use common::{PACKAGE, assert_script_fails_with, typecheck_script as typecheck};

#[test]
fn happy_path_impl_satisfies_protocol() {
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Point
          x: Int
          y: Int
        end

        impl Greeter for Point
          fn greet(self) -> String
            \"Point\"
          end
        end

        p = Point{x: 1, y: 2}
        p.greet()
        ";

    let checked = typecheck(&dedent(source));
    let method_ident = Identifier::new(PACKAGE, vec!["Point".to_string(), "greet".to_string()]);
    let (_, entry) = checked
        .registry
        .lookup(&method_ident)
        .expect("Point.greet should be registered");
    assert!(matches!(entry.kind, GlobalKind::Function(Some(_))));
}

#[test]
fn default_body_synthesizes_when_impl_omits() {
    let source = "
        protocol Labeler
          fn label(self) -> String
            \"default\"
          end
        end

        struct Tag
        end

        impl Labeler for Tag
        end
        ";

    let checked = typecheck(&dedent(source));
    let method_ident = Identifier::new(PACKAGE, vec!["Tag".to_string(), "label".to_string()]);
    let (_, entry) = checked
        .registry
        .lookup(&method_ident)
        .expect("synthesized Tag.label should be registered");
    assert!(matches!(entry.kind, GlobalKind::Function(Some(_))));

    // The synthesized Function should also be present in the impl
    // block's members so resolve walks it like any other method.
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("test package missing");
    let mut found = false;
    for file in &pkg.files {
        for item in &file.items {
            let Item::Impl(impl_block) = item else {
                continue;
            };
            for member in &impl_block.members {
                if let koja_ast::ast::ImplMember::Function(function) = member
                    && function.name == "label"
                {
                    assert_eq!(function.visibility, Visibility::Public);
                    found = true;
                }
            }
        }
    }
    assert!(found, "synthesized `label` not found on impl block members");
}

#[test]
fn default_body_does_not_synthesize_when_impl_overrides() {
    let source = "
        protocol Labeler
          fn label(self) -> String
            \"default\"
          end
        end

        struct Tag
        end

        impl Labeler for Tag
          fn label(self) -> String
            \"override\"
          end
        end
        ";

    let checked = typecheck(&dedent(source));
    // Exactly one `label` method should be on the impl members, no
    // duplicate from synthesis.
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .unwrap();
    let mut count = 0;
    for file in &pkg.files {
        for item in &file.items {
            let Item::Impl(impl_block) = item else {
                continue;
            };
            for member in &impl_block.members {
                if let koja_ast::ast::ImplMember::Function(function) = member
                    && function.name == "label"
                {
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 1, "expected exactly one `label` impl method");
}

#[test]
fn missing_required_method_diagnoses() {
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Point
        end

        impl Greeter for Point
        end
        ";

    assert_script_fails_with(source, &["missing method `greet`", "Greeter"]);
}

#[test]
fn extra_impl_method_diagnoses() {
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Point
        end

        impl Greeter for Point
          fn greet(self) -> String
            \"hi\"
          end

          fn extra(self) -> String
            \"surprise\"
          end
        end
        ";

    assert_script_fails_with(source, &["`extra`", "not declared in protocol", "Greeter"]);
}

#[test]
fn priv_helper_in_protocol_impl_is_allowed() {
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Robot
          name: String
        end

        impl Greeter for Robot
          fn greet(self) -> String
            self.prefix() <> self.name
          end

          priv fn prefix(self) -> String
            \"beep boop \"
          end
        end

        r = Robot{name: \"R2\"}
        r.greet()
        ";

    let checked = typecheck(&dedent(source));
    let helper_ident = Identifier::new(PACKAGE, vec!["Robot".to_string(), "prefix".to_string()]);
    let (_, entry) = checked
        .registry
        .lookup(&helper_ident)
        .expect("Robot.prefix should be registered as a type-private helper");
    assert!(matches!(entry.kind, GlobalKind::Function(Some(_))));
}

#[test]
fn return_type_mismatch_diagnoses() {
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Point
        end

        impl Greeter for Point
          fn greet(self) -> Int
            0
          end
        end
        ";

    assert_script_fails_with(source, &["return type", "`greet`"]);
}

#[test]
fn param_type_mismatch_diagnoses() {
    let source = "
        protocol Combiner
          fn join(self, n: Int) -> Int
        end

        struct Adder
        end

        impl Combiner for Adder
          fn join(self, n: String) -> Int
            0
          end
        end
        ";

    assert_script_fails_with(source, &["param", "`n`", "`join`"]);
}

#[test]
fn protocol_impl_accepts_equivalent_alias_parameter() {
    let source = "
        type Count = Int

        protocol Combiner
          fn join(self, n: Count) -> Int
        end

        struct Adder
        end

        impl Combiner for Adder
          fn join(self, n: Int) -> Int
            n
          end
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn protocol_impl_accepts_int64_for_int_parameter() {
    let source = "
        protocol Combiner
          fn join(self, n: Int) -> Int
        end

        struct Adder
        end

        impl Combiner for Adder
          fn join(self, n: Int64) -> Int
            0
          end
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn protocol_impl_accepts_int_for_int64_return() {
    let source = "
        protocol Counter
          fn count(self) -> Int64
        end

        struct Tally
        end

        impl Counter for Tally
          fn count(self) -> Int
            0
          end
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn dispatch_mismatch_diagnoses() {
    let source = "
        protocol Maker
          fn make() -> Int
        end

        struct Factory
        end

        impl Maker for Factory
          fn make(self) -> Int
            0
          end
        end
        ";

    assert_script_fails_with(source, &["receiver shape", "`make`"]);
}

#[test]
fn inherent_and_trait_impl_collide_on_same_method_name() {
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Point
        end

        extend Point
          fn greet(self) -> String
            \"inherent\"
          end
        end

        impl Greeter for Point
          fn greet(self) -> String
            \"trait\"
          end
        end
        ";

    assert_script_fails_with(source, &["already defined", "greet"]);
}
