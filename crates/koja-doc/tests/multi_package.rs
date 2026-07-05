//! Integration tests for the package-aware doc generator.
//!
//! Covers the full extract -> finalize -> render -> search-index
//! pipeline across multiple packages, including the
//! sort-by-tier-then-name guarantee, the per-package subdir
//! URLs in the search index, and the rendered output's wiring
//! of the new sidebar (search input, package dropdown, item
//! list).

use koja_ast::util::dedent;
use koja_doc::{
    DocProject, PackageKind, extract_items, finalize_project, render_package_index,
    render_root_index, render_struct, search_index_json,
};
use koja_parser::ParseMode;

fn parse(src: &str) -> koja_ast::ast::File {
    let result = koja_parser::parse(&dedent(src), ParseMode::File);
    assert!(
        result.errors.is_empty(),
        "parse errors: {:?}",
        result.errors
    );
    result.ast
}

fn ingest(project: &mut DocProject, package: &str, kind: PackageKind, src: &str) {
    let ast = parse(src);
    extract_items(&ast, project, package, kind);
}

fn build_project() -> DocProject {
    let mut project = DocProject::new("MyApp");

    ingest(
        &mut project,
        "MyApp",
        PackageKind::Project,
        "
        @doc \"A counter for the app.\"
        struct Counter
          count: Int
        end

        extend Counter
          @doc \"Bump the counter by one.\"
          fn bump
            self.count + 1
          end
        end

        @doc \"Top-level helper.\"
        fn greet(name: String) -> String
          \"hi, #{name}\"
        end
        ",
    );

    ingest(
        &mut project,
        "Crypto",
        PackageKind::Stdlib,
        "
        @doc \"SHA-256 hasher.\"
        struct SHA256
          state: List<Int>
        end

        extend SHA256
          @doc \"Finalize the digest.\"
          fn digest
            self.state
          end
        end
        ",
    );

    ingest(
        &mut project,
        "Global",
        PackageKind::Stdlib,
        "
        @doc \"A generic list.\"
        struct List<T>
          items: T
        end
        ",
    );

    ingest(
        &mut project,
        "Helper",
        PackageKind::Dependency,
        "
        @doc \"Helper utility.\"
        fn assist
          1
        end
        ",
    );

    finalize_project(&mut project);
    project
}

#[test]
fn packages_sort_project_then_deps_then_stdlib() {
    let project = build_project();
    let names: Vec<&str> = project.packages.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["MyApp", "Helper", "Crypto", "Global"]);
}

#[test]
fn items_sort_alphabetically_within_each_package() {
    let project = build_project();
    let crypto = project.find_package("Crypto").expect("Crypto present");
    let crypto_names: Vec<&str> = crypto.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(crypto_names, vec!["SHA256"]);

    let myapp = project.find_package("MyApp").expect("MyApp present");
    let myapp_names: Vec<&str> = myapp.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(myapp_names, vec!["Counter", "greet"]);
}

#[test]
fn search_index_includes_items_and_methods() {
    let project = build_project();
    let json = search_index_json(&project);

    assert!(json.contains("\"pkg\":\"MyApp\""));
    assert!(json.contains("\"name\":\"Counter\""));
    assert!(json.contains("\"url\":\"MyApp/Counter.html\""));
    assert!(json.contains("\"name\":\"Counter.bump\""));
    assert!(json.contains("\"url\":\"MyApp/Counter.html#fn-bump\""));
    assert!(json.contains("\"pkg\":\"Crypto\""));
    assert!(json.contains("\"url\":\"Crypto/SHA256.html#fn-digest\""));
    assert!(json.contains("\"pkg\":\"Helper\""));
    assert!(json.contains("\"url\":\"Helper/assist.html\""));
}

#[test]
fn root_index_renders_package_roster() {
    let project = build_project();
    let html = render_root_index(&project);

    assert!(html.contains("MyApp/index.html"));
    assert!(html.contains("Crypto/index.html"));
    assert!(html.contains("Global/index.html"));
    assert!(html.contains("Helper/index.html"));
    assert!(html.contains("chip-project"));
    assert!(html.contains("chip-stdlib"));
    assert!(html.contains("chip-dependency"));
    assert!(html.contains("id=\"doc-search\""));
    assert!(html.contains("data-root-prefix=\"\""));
}

#[test]
fn package_index_links_back_to_root_assets() {
    let project = build_project();
    let crypto = project.find_package("Crypto").expect("Crypto present");
    let html = render_package_index(crypto, &project);

    assert!(html.contains("data-root-prefix=\"../\""));
    assert!(html.contains("href=\"../style.css\""));
    assert!(html.contains("src=\"../search.js\""));
    assert!(html.contains("href=\"SHA256.html\""));
    assert!(html.contains("class=\"sidebar-package\""));
    assert!(html.contains("value=\"../MyApp/index.html\""));
}

#[test]
fn private_functions_are_hidden_everywhere() {
    let mut project = DocProject::new("Vis");
    ingest(
        &mut project,
        "Vis",
        PackageKind::Project,
        "
        priv fn helper_top
          1
        end

        fn public_top
          1
        end

        struct Container
          count: Int

          priv fn helper_decl
            self.count
          end

          fn public_decl
            self.count
          end
        end

        enum Color
          Red
          Blue

          priv fn helper_enum
            1
          end

          fn public_enum
            2
          end
        end

        extend Container
          priv fn helper_impl
            self.count
          end

          fn public_impl
            self.count
          end
        end
        ",
    );
    finalize_project(&mut project);

    let vis = project.find_package("Vis").expect("Vis present");

    let fn_names: Vec<&str> = vis.functions.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(fn_names, vec!["public_top"], "top-level priv fn leaked");

    let container = vis
        .structs
        .iter()
        .find(|s| s.name == "Container")
        .expect("Container");
    let methods: Vec<&str> = container
        .functions
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(
        methods,
        vec!["public_decl", "public_impl"],
        "priv decl-block or impl fn leaked: {methods:?}"
    );

    let color = vis.enums.iter().find(|e| e.name == "Color").expect("Color");
    let enum_methods: Vec<&str> = color.functions.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        enum_methods,
        vec!["public_enum"],
        "priv enum decl-block fn leaked: {enum_methods:?}"
    );
}

#[test]
fn struct_page_links_methods_and_other_packages() {
    let project = build_project();
    let crypto = project.find_package("Crypto").expect("Crypto present");
    let sha = crypto
        .structs
        .iter()
        .find(|s| s.name == "SHA256")
        .expect("SHA256");
    let html = render_struct(sha, crypto, &project);

    assert!(html.contains("id=\"fn-digest\""));
    assert!(html.contains("href=\"#fn-digest\""));
    assert!(html.contains("value=\"../MyApp/index.html\""));
    assert!(html.contains("data-root-prefix=\"../\""));
}
