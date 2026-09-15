//! Symbol lookup, `@doc` retrieval, and rename planning over a
//! typechecked program. Positions are 1-indexed `(line, col)` into
//! the dedented source.

use std::path::PathBuf;

use koja_ast::span::FileId;
use koja_ast::util::dedent;
use koja_parser::{ParseMode, SourceFile, parse_program};
use koja_query::rename::{RenameRefusal, prepare_rename, validate_new_name};
use koja_query::symbol::{Symbol, SymbolKind, symbol_at};
use koja_query::{Analysis, ReferenceIndex, SymbolKey, docs};
use koja_typecheck::{CheckFailure, CheckedProgram, GlobalKind, check_program};

const PACKAGE: &str = "TestApp";
const MAIN: &str = "main.koja";

fn sources(files: &[(&str, &str)]) -> Vec<SourceFile> {
    let mut sources = koja_stdlib::autoimport_sources();
    sources.extend(koja_stdlib::qualified_sources());
    for (path, source) in files {
        sources.push(SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from(path),
            source: dedent(source),
        });
    }
    sources
}

fn check(source: &str) -> CheckedProgram {
    let parsed = parse_program(sources(&[(MAIN, source)]), ParseMode::File);
    check_program(parsed).unwrap_or_else(|failure| {
        let messages: Vec<&str> = failure
            .diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect();
        panic!("typecheck failed: {messages:?}")
    })
}

fn fail(source: &str) -> CheckFailure {
    let parsed = parse_program(sources(&[(MAIN, source)]), ParseMode::File);
    check_program(parsed).expect_err("typecheck should fail")
}

/// Index and cursor helpers over one analysis.
struct Fixture<'a> {
    analysis: &'a Analysis<'a>,
    index: ReferenceIndex,
    main: FileId,
}

impl<'a> Fixture<'a> {
    fn new(analysis: &'a Analysis<'a>) -> Self {
        let index = ReferenceIndex::build_filtered(analysis, |file| file.package == PACKAGE);
        let main = analysis.file_id(&PathBuf::from(MAIN)).expect("main file");
        Self {
            analysis,
            index,
            main,
        }
    }

    fn symbol(&self, line: u32, col: u32) -> Option<Symbol<'a>> {
        symbol_at(self.analysis, &self.index, self.main, line, col)
    }

    fn key(&self, line: u32, col: u32) -> SymbolKey {
        self.symbol(line, col)
            .unwrap_or_else(|| panic!("no symbol at {line}:{col}"))
            .key
    }

    fn rename(&self, line: u32, col: u32) -> Result<Vec<(u32, u32)>, RenameRefusal> {
        let is_project = |file: FileId| file == self.main;
        prepare_rename(self.analysis, &self.index, self.key(line, col), is_project).map(|rename| {
            let mut spans: Vec<(u32, u32)> = rename
                .spans
                .iter()
                .map(|s| (s.start.line, s.start.column))
                .collect();
            spans.sort_unstable();
            spans
        })
    }
}

#[test]
fn keywords_inside_a_struct_hit_no_symbol() {
    // Derived `Debug` / `Equality` impls reuse the declaring type's
    // span on every node. Keywords and whitespace must not fall into
    // them.
    let checked = check(
        r#"
        priv struct Wire
          priv fn scan(data: Binary) -> Option<(String, Binary)>
            Option.None
          end
        end
        "#,
    );
    let analysis = Analysis::from_checked(&checked);
    let f = Fixture::new(&analysis);
    assert!(f.symbol(2, 3).is_none(), "`priv` is not a symbol");
    assert!(f.symbol(3, 3).is_none(), "indentation is not a symbol");
    let option = f.symbol(3, 5).expect("Option in the body");
    assert_eq!(option.name, "Option");
    assert!(matches!(
        option.kind,
        SymbolKind::Global(entry) if matches!(entry.kind, GlobalKind::Enum(_))
    ));
}

#[test]
fn header_conformance_resolves_to_the_protocol() {
    let checked = check(
        r#"
        protocol Marker
          fn tag(self) -> String
        end

        struct Point: Marker
          x: Int

          fn tag(self) -> String
            "point"
          end
        end

        enum Flag: Marker
          On
          Off

          fn tag(self) -> String
            "flag"
          end
        end
        "#,
    );
    let analysis = Analysis::from_checked(&checked);
    let f = Fixture::new(&analysis);
    let declaration = f.key(1, 10);
    for (line, col) in [(5, 15), (13, 12)] {
        let symbol = f.symbol(line, col).expect("symbol on the header entry");
        assert_eq!(symbol.key, declaration);
        assert!(matches!(
            symbol.kind,
            SymbolKind::Global(entry) if matches!(entry.kind, GlobalKind::Protocol(_))
        ));
    }
}

#[test]
fn local_symbol_carries_the_declared_type() {
    let checked = check(
        r#"
        fn add(a: Int, b: Int) -> Int
          total = a + b
          total
        end
        "#,
    );
    let analysis = Analysis::from_checked(&checked);
    let f = Fixture::new(&analysis);
    let total = f.symbol(3, 3).expect("read of total");
    assert_eq!(total.name, "total");
    let SymbolKind::Local { ty: Some(ty) } = total.kind else {
        panic!("local with a type, got {:?}", total.kind);
    };
    assert_eq!(
        koja_query::display::format_resolved_type(&ty, analysis.registry),
        "Int"
    );
    let a = f.symbol(1, 8).expect("parameter a");
    assert!(matches!(a.kind, SymbolKind::Local { ty: Some(_) }));
}

#[test]
fn doc_for_finds_declarations_by_name_span() {
    let checked = check(
        r#"
        @doc """
        Adds two numbers.
        """
        fn add(a: Int, b: Int) -> Int
          a + b
        end

        struct Greeter
          @doc """
          Says hello.
          """
          fn hello()
          end
        end

        protocol Greet
          @doc """
          Greeting verb.
          """
          fn hello(self) -> String
        end

        extend Greeter
          @doc """
          Bumps.
          """
          fn bump(self)
          end
        end
        "#,
    );
    let analysis = Analysis::from_checked(&checked);
    let f = Fixture::new(&analysis);
    let doc_at = |line, col| {
        let SymbolKey::Global(id) = f.key(line, col) else {
            panic!("global at {line}:{col}");
        };
        docs::doc_for(&analysis, id).unwrap_or_else(|| panic!("doc at {line}:{col}"))
    };
    assert!(doc_at(4, 4).contains("Adds two numbers."));
    assert!(doc_at(12, 6).contains("Says hello."));
    assert!(doc_at(27, 6).contains("Bumps."));
    // Stdlib `Int` has a doc too, found in its own file.
    assert!(doc_at(4, 11).contains("integer"));
}

#[test]
fn doc_for_reads_builtins_and_inline_builtin_methods() {
    let checked = check("fn len(s: String) -> Int\n  s.length()\nend\n");
    let analysis = Analysis::from_checked(&checked);
    let f = Fixture::new(&analysis);
    let SymbolKey::Global(string_id) = f.key(1, 11) else {
        panic!("String is a global");
    };
    assert!(docs::doc_for(&analysis, string_id).is_some());
    let SymbolKey::Global(length_id) = f.key(2, 5) else {
        panic!("length is a global");
    };
    assert!(docs::doc_for(&analysis, length_id).is_some());
}

#[test]
fn rename_collects_every_span_of_a_local_and_a_function() {
    let checked = check(
        r#"
        fn helper(n: Int) -> Int
          doubled = n + n
          doubled
        end

        fn run() -> Int
          helper(2) + helper(3)
        end
        "#,
    );
    let analysis = Analysis::from_checked(&checked);
    let f = Fixture::new(&analysis);
    assert_eq!(f.rename(3, 3), Ok(vec![(2, 3), (3, 3)]));
    assert_eq!(f.rename(7, 3), Ok(vec![(1, 4), (7, 3), (7, 15)]));
    assert_eq!(f.rename(1, 11), Ok(vec![(1, 11), (2, 13), (2, 17)]));
}

#[test]
fn rename_refuses_while_the_program_has_errors() {
    let failure = fail(
        r#"
        fn broken() -> Int
          "no"
        end

        fn fine(x: Int) -> Int
          x
        end
        "#,
    );
    let analysis = Analysis::from_failure(&failure).expect("registry present");
    let f = Fixture::new(&analysis);
    assert!(f.symbol(6, 3).is_some(), "navigation still works");
    assert_eq!(f.rename(6, 3), Err(RenameRefusal::ProgramHasErrors));
}

#[test]
fn rename_refuses_stdlib_builtins_self_and_protocol_methods() {
    let checked = check(
        r#"
        protocol Marker
          fn tag(self) -> String
        end

        struct Point: Marker
          x: Int

          fn tag(self) -> String
            self.x.format()
          end

          fn own(self) -> Int
            self.x
          end
        end

        fn first(items: List<Int>) -> Option<Int>
          items.first()
        end
        "#,
    );
    let analysis = Analysis::from_checked(&checked);
    let f = Fixture::new(&analysis);
    // `Int` is a builtin declared in the stdlib.
    assert!(matches!(
        f.rename(6, 6),
        Err(RenameRefusal::DeclaredOutsideProject | RenameRefusal::NotRenamable(_))
    ));
    // `Option` is a stdlib enum.
    assert_eq!(f.rename(17, 31), Err(RenameRefusal::DeclaredOutsideProject));
    // `first` on `List` is declared in the stdlib.
    assert_eq!(f.rename(18, 9), Err(RenameRefusal::DeclaredOutsideProject));
    // `self` is a keyword.
    assert_eq!(f.rename(9, 5), Err(RenameRefusal::NotRenamable("`self`")));
    // `tag` implements `Marker.tag`.
    assert!(matches!(
        f.rename(8, 6),
        Err(RenameRefusal::NotRenamable(_))
    ));
    // `own` is inherent and renames with its declaration only.
    assert_eq!(f.rename(12, 6), Ok(vec![(12, 6)]));
    // A project struct renames across its declaration and uses.
    assert_eq!(f.rename(5, 8), Ok(vec![(5, 8)]));
}

#[test]
fn rename_refuses_a_symbol_referenced_through_an_alias() {
    let checked = check(
        r#"
        alias Process.ExitSignal as Signal

        fn wants(s: Signal) -> Signal
          s
        end
        "#,
    );
    let analysis = Analysis::from_checked(&checked);
    let f = Fixture::new(&analysis);
    // The alias use site resolves to `Process.ExitSignal`, which is
    // stdlib and aliased, so both refusals are acceptable.
    assert!(matches!(
        f.rename(3, 13),
        Err(RenameRefusal::DeclaredOutsideProject | RenameRefusal::Aliased)
    ));
}

#[test]
fn new_name_must_match_the_old_case_class() {
    assert_eq!(validate_new_name("total", "sum"), Ok(()));
    assert_eq!(validate_new_name("empty?", "blank?"), Ok(()));
    assert_eq!(validate_new_name("Point", "Vec2"), Ok(()));
    assert!(validate_new_name("total", "").is_err());
    assert!(validate_new_name("total", "Total").is_err());
    assert!(validate_new_name("Point", "point").is_err());
    assert!(validate_new_name("Point", "Point?").is_err());
    assert!(validate_new_name("total", "1st").is_err());
    assert!(validate_new_name("total", "two words").is_err());
    assert!(validate_new_name("total", "end").is_err());
    assert!(validate_new_name("total", "self").is_err());
}
