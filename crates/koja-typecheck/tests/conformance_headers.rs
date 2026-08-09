//! Typecheck coverage for inline conformance headers
//! (`struct T: P, Q` / `enum E: P`): happy paths, conformance
//! recording, default-method synthesis into the type body,
//! diagnostics (missing member, signature mismatch, duplicates,
//! non-protocol entry), and the near-miss warning.

use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_typecheck::GlobalKind;

mod common;

use common::{
    PACKAGE, assert_script_fails_with, enum_definition, find_struct_decl, registry_id,
    struct_definition, typecheck_script as typecheck, warning_messages,
};

#[test]
fn struct_header_records_conformance() {
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Point: Greeter
          x: Int

          fn greet(self) -> String
            \"Point\"
          end
        end

        Point{x: 1}.greet()
        ";

    let checked = typecheck(&dedent(source));
    let protocol_id = registry_id(&checked, PACKAGE, &["Greeter"]);
    assert!(
        struct_definition(&checked, "Point")
            .conformances
            .contains_key(&protocol_id)
    );
}

#[test]
fn enum_header_records_conformance() {
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        enum Direction: Greeter
          North
          South

          fn greet(self) -> String
            \"direction\"
          end
        end

        Direction.North.greet()
        ";

    let checked = typecheck(&dedent(source));
    let protocol_id = registry_id(&checked, PACKAGE, &["Greeter"]);
    assert!(
        enum_definition(&checked, "Direction")
            .conformances
            .contains_key(&protocol_id)
    );
}

#[test]
fn generic_struct_header_with_protocol_args_referencing_params() {
    let source = "
        protocol Container<T>
          fn first(self) -> Option<T>
        end

        struct Bag<T>: Container<T>
          items: List<T>

          fn first(self) -> Option<T>
            self.items.get(0)
          end
        end

        bag = Bag{items: [1, 2]}
        bag.first()
        ";

    let checked = typecheck(&dedent(source));
    let protocol_id = registry_id(&checked, PACKAGE, &["Container"]);
    assert!(
        struct_definition(&checked, "Bag")
            .conformances
            .contains_key(&protocol_id)
    );
}

#[test]
fn header_synthesizes_default_method_into_type_body() {
    let source = "
        protocol Greeter
          fn greet(self) -> String

          fn excited(self) -> String
            \"#{self.greet()}!\"
          end
        end

        struct Point: Greeter
          x: Int

          fn greet(self) -> String
            \"Point\"
          end
        end

        Point{x: 1}.excited()
        ";

    let checked = typecheck(&dedent(source));
    let method_ident = Identifier::new(PACKAGE, vec!["Point".to_string(), "excited".to_string()]);
    let (_, entry) = checked
        .registry
        .lookup(&method_ident)
        .expect("synthesized Point.excited should be registered");
    assert!(matches!(entry.kind, GlobalKind::Function(Some(_))));
    let decl = find_struct_decl(&checked, "Point");
    assert!(
        decl.functions.iter().any(|f| f.name == "excited"),
        "synthesized `excited` should land in the type body's functions"
    );
}

#[test]
fn one_body_fn_satisfies_two_protocols() {
    let source = "
        protocol Reader
          fn tag(self) -> String
        end

        protocol Writer
          fn tag(self) -> String
        end

        struct File: Reader, Writer
          name: String

          fn tag(self) -> String
            self.name
          end
        end

        File{name: \"log\"}.tag()
        ";

    let checked = typecheck(&dedent(source));
    let conformances = &struct_definition(&checked, "File").conformances;
    assert!(conformances.contains_key(&registry_id(&checked, PACKAGE, &["Reader"])));
    assert!(conformances.contains_key(&registry_id(&checked, PACKAGE, &["Writer"])));
}

#[test]
fn header_missing_method_fails() {
    assert_script_fails_with(
        "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Point: Greeter
          x: Int
        end
        ",
        &[
            "missing method `greet` required by protocol",
            "declared on `struct Point`",
        ],
    );
}

#[test]
fn header_signature_mismatch_fails() {
    assert_script_fails_with(
        "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Point: Greeter
          x: Int

          fn greet(self) -> Int
            self.x
          end
        end
        ",
        &["return type of method `greet` does not match protocol"],
    );
}

#[test]
fn duplicate_entry_within_header_fails() {
    assert_script_fails_with(
        "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Point: Greeter, Greeter
          x: Int

          fn greet(self) -> String
            \"Point\"
          end
        end
        ",
        &["duplicate conformance", "declared on `struct Point`"],
    );
}

#[test]
fn header_doubled_by_impl_block_fails() {
    assert_script_fails_with(
        "
        protocol Greeter
          fn greet(self) -> String
        end

        struct Point: Greeter
          x: Int

          fn greet(self) -> String
            \"Point\"
          end
        end

        impl Greeter for Point
          fn greet(self) -> String
            \"again\"
          end
        end
        ",
        &["duplicate `impl"],
    );
}

#[test]
fn header_entry_must_be_a_protocol() {
    assert_script_fails_with(
        "
        struct Other
          y: Int
        end

        struct Point: Other
          x: Int
        end
        ",
        &["requires a protocol"],
    );
}

#[test]
fn near_miss_of_omitted_default_warns() {
    let source = "
        protocol Greeter
          fn greet(self) -> String

          fn excited(self) -> String
            \"#{self.greet()}!\"
          end
        end

        struct Point: Greeter
          x: Int

          fn greet(self) -> String
            \"Point\"
          end

          fn exicted(self) -> String
            \"typo\"
          end
        end

        Point{x: 1}.excited()
        ";

    let checked = typecheck(&dedent(source));
    let warnings = warning_messages(&checked);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("`exicted` does not override")),
        "expected near-miss warning, got: {warnings:#?}"
    );
}
