//! Source-to-sealed-IR coverage for transparent tuple aliases.

mod common;

use common::lower_script_source;

#[test]
fn tuple_aliases_lower_across_methods_patterns_elements_and_generics() {
    let source = "
        type Label = String
        type Pair = (Int, Label)
        type NamedPair = Pair
        type Coordinates = (Int, Int)

        fn make(label: Label) -> Pair
          (1, label)
        end

        fn render<T: Debug>(value: T) -> String
          value.format()
        end

        left: NamedPair = make(\"one\")
        right: Pair = make(\"one\")
        left.print()
        (left == right).print()
        IO.puts(make(\"name\").format())
        IO.puts(render(left))

        (number, label) = left
        IO.puts(\"#{number}=#{label}\")

        match right
          (matched, _) -> matched.print()
        end

        coordinates: Coordinates = (2, 3)
        (coordinates, \"point\").print()
        ";

    lower_script_source(source);
}
