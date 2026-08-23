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
    DocProject, PackageKind, extract_items, finalize_project, render_enum, render_function,
    render_package_index, render_root_index, render_struct, search_index_json, terminal,
    terminal::SearchOutcome,
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

#[test]
fn overloads_keep_distinct_pages_and_member_anchors() {
    let mut project = DocProject::new("MyApp");
    ingest(
        &mut project,
        "MyApp",
        PackageKind::Project,
        "
        fn choose(value: Int) -> Int
          value
        end

        fn choose(left: Int, right: Int) -> Int
          left + right
        end

        struct Counter
          value: Int

          fn add(self, value: Int) -> Int
            self.value + value
          end

          fn add(self, left: Int, right: Int) -> Int
            self.value + left + right
          end
        end
        ",
    );
    finalize_project(&mut project);

    let package = &project.packages[0];
    assert_eq!(package.functions.len(), 2);
    assert_eq!(package.functions[0].page_name(), "choose-arity-1");
    assert_eq!(package.functions[1].page_name(), "choose-arity-2");
    let first_page = render_function(&package.functions[0], package, &project);
    assert!(first_page.contains("choose"));

    let counter = &package.structs[0];
    let html = render_struct(counter, package, &project);
    assert!(html.contains("fn-add-arity-2"));
    assert!(html.contains("fn-add-arity-3"));
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
    assert_eq!(myapp_names, vec!["Counter", "greet/1"]);
}

#[test]
fn nested_types_keep_full_names_and_deprecation_metadata() {
    let mut project = DocProject::new("Net");
    ingest(
        &mut project,
        "Net",
        PackageKind::Project,
        "
        @doc \"An IP address.\"
        struct IPAddress
          bytes: Binary

          @doc \"The address version.\"
          @deprecated \"Use `AddressFamily` instead.\"
          enum Version
            V4
            V6
          end

          @doc false
          struct Internal
            @doc \"A visible child of a hidden owner page.\"
            enum Child
              One
            end
          end

          priv struct Hidden
            enum Secret
              One
            end
          end
        end

        @doc \"An invalid address.\"
        enum IPAddress.ParseError
          Invalid(String)
        end

        extend IPAddress.Version
          @doc \"Returns the old version label.\"
          @deprecated \"Use `label` instead.\"
          fn legacy(self) -> String
            \"old\"
          end
        end
        ",
    );
    ingest(
        &mut project,
        "Addon",
        PackageKind::Dependency,
        "
        extend Net.IPAddress.Version
          @doc \"Returns an external label.\"
          fn external_label(self) -> String
            \"external\"
          end
        end
        ",
    );
    finalize_project(&mut project);

    let net = project.find_package("Net").expect("Net present");
    let names: Vec<&str> = net.items.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "IPAddress",
            "IPAddress.Internal.Child",
            "IPAddress.ParseError",
            "IPAddress.Version",
        ]
    );

    let version = net
        .enums
        .iter()
        .find(|item| item.name == "IPAddress.Version")
        .expect("nested version enum");
    assert_eq!(
        version.deprecated.as_deref(),
        Some("Use `AddressFamily` instead.")
    );
    let functions: Vec<&str> = version
        .functions
        .iter()
        .map(|function| function.name.as_str())
        .collect();
    assert_eq!(functions, vec!["external_label", "legacy"]);
    assert_eq!(
        version.functions[1].deprecated.as_deref(),
        Some("Use `label` instead.")
    );

    let html = render_enum(version, net, &project);
    assert!(html.contains("IPAddress.Version"));
    assert!(html.contains("class=\"deprecation-notice\""));
    assert!(html.contains("Use <code>AddressFamily</code> instead."));
    assert!(html.contains("Use <code>label</code> instead."));
    let legacy_notice = html.find("Use <code>label</code> instead.").unwrap();
    let legacy_signature = html.find("<span class=\"fn\">legacy</span>").unwrap();
    assert!(legacy_notice < legacy_signature);

    let package_html = render_package_index(net, &project);
    assert!(package_html.contains("href=\"IPAddress.Version.html\""));
    assert!(package_html.contains("class=\"deprecated-badge\">deprecated</span>"));

    let json = search_index_json(&project);
    assert!(json.contains("\"name\":\"IPAddress.Version\""));
    assert!(json.contains("\"url\":\"Net/IPAddress.Version.html\""));
    assert!(json.contains("\"deprecated\":\"Use `AddressFamily` instead.\""));
    assert!(json.contains("\"name\":\"IPAddress.ParseError\""));
    assert!(json.contains("\"deprecated\":null"));

    let SearchOutcome::Hits(version_doc) = terminal::search(&project, "IPAddress.Version") else {
        panic!("expected nested type hit");
    };
    assert!(version_doc.contains("> **Deprecated**\n>\n> Use `AddressFamily` instead."));
    assert!(version_doc.contains("### `fn legacy(self) -> String`"));
    assert!(version_doc.contains("> **Deprecated**\n>\n> Use `label` instead."));

    let SearchOutcome::Hits(legacy_doc) = terminal::search(&project, "IPAddress.Version.legacy")
    else {
        panic!("expected deprecated function hit");
    };
    assert!(legacy_doc.contains("> **Deprecated**\n>\n> Use `label` instead."));
    assert!(!legacy_doc.contains("## Deprecated"));

    let SearchOutcome::Hits(version_list) = terminal::search(&project, "Version") else {
        panic!("expected nested type list");
    };
    assert!(version_list.contains("- Net.IPAddress.Version (enum, deprecated)"));
}

#[test]
fn search_index_includes_items_and_methods() {
    let project = build_project();
    let json = search_index_json(&project);

    assert!(json.contains("\"pkg\":\"MyApp\""));
    assert!(json.contains("\"name\":\"Counter\""));
    assert!(json.contains("\"url\":\"MyApp/Counter.html\""));
    assert!(json.contains("\"name\":\"Counter.bump/0\""));
    assert!(json.contains("\"url\":\"MyApp/Counter.html#fn-bump-arity-0\""));
    assert!(json.contains("\"pkg\":\"Crypto\""));
    assert!(json.contains("\"url\":\"Crypto/SHA256.html#fn-digest-arity-0\""));
    assert!(json.contains("\"pkg\":\"Helper\""));
    assert!(json.contains("\"url\":\"Helper/assist-arity-0.html\""));
}

#[test]
fn root_index_renders_package_roster() {
    let project = build_project();
    let html = render_root_index(&project);

    assert!(html.contains("MyApp/index.html"));
    assert!(html.contains("Crypto/index.html"));
    assert!(html.contains("Global/index.html"));
    assert!(html.contains("Helper/index.html"));
    assert!(html.contains("<span class=\"item-kind\">project</span>"));
    assert!(html.contains("<span class=\"item-kind\">stdlib</span>"));
    assert!(html.contains("<span class=\"item-kind\">dependency</span>"));
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
    assert!(html.contains("src=\"../doc.js\""));
    assert!(html.contains("href=\"SHA256.html\""));
    assert!(html.contains("class=\"package-select\""));
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
fn private_decls_are_hidden_everywhere() {
    let mut project = DocProject::new("Vis");
    ingest(
        &mut project,
        "Vis",
        PackageKind::Project,
        "
        priv struct HiddenStruct
          value: Int
        end

        struct OpenStruct
          value: Int
        end

        priv enum HiddenEnum
          One
        end

        enum OpenEnum
          One
        end

        priv protocol HiddenProto
          fn op(self) -> Int
        end

        protocol OpenProto
          fn op(self) -> Int
        end

        priv const HIDDEN_LIMIT: Int = 1

        const OPEN_LIMIT: Int = 1

        extend HiddenStruct
          fn poke(self) -> Int
            self.value
          end
        end
        ",
    );
    finalize_project(&mut project);

    let vis = project.find_package("Vis").expect("Vis present");
    let item_names: Vec<&str> = vis.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(
        item_names,
        vec!["OPEN_LIMIT", "OpenEnum", "OpenProto", "OpenStruct"],
        "a priv decl leaked into the doc roster"
    );

    // The extend on the undocumented private target is dropped, not
    // misrouted onto some other item.
    assert!(
        vis.structs.iter().all(|s| s.functions.is_empty()),
        "extend methods on a priv target leaked"
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

    assert!(html.contains("id=\"fn-digest-arity-0\""));
    assert!(html.contains("href=\"#fn-digest-arity-0\""));
    assert!(html.contains("value=\"../MyApp/index.html\""));
    assert!(html.contains("data-root-prefix=\"../\""));
    // Signatures render pre-highlighted.
    assert!(html.contains("<span class=\"kw\">fn</span>"));
    // Even a small page renders its "on this page" TOC.
    assert!(html.contains("On this page"));
}

#[test]
fn terminal_search_covers_exact_partial_and_none() {
    let project = build_project();

    let SearchOutcome::Hits(full) = terminal::search(&project, "counter") else {
        panic!("expected exact hit");
    };
    assert!(full.starts_with("# MyApp.Counter (struct)\n"));
    assert!(full.contains("A counter for the app."));
    assert!(full.contains("### `fn bump()`"));
    assert!(
        full.contains("## Also matched\n\n- MyApp.Counter.bump/0 (fn): Bump the counter by one.\n")
    );

    let SearchOutcome::Hits(function) = terminal::search(&project, "Counter.bump") else {
        panic!("expected function hit");
    };
    assert!(function.starts_with("# MyApp.Counter.bump/0 (fn)\n"));
    assert!(function.contains("```koja\nfn bump()\n```"));

    let SearchOutcome::Hits(list) = terminal::search(&project, "s") else {
        panic!("expected partial hits");
    };
    assert!(list.contains("- Crypto.SHA256 (struct): SHA-256 hasher."));
    assert!(list.contains("- Helper.assist/0 (fn): Helper utility."));

    assert!(matches!(
        terminal::search(&project, "nope"),
        SearchOutcome::NoMatches
    ));
}

#[test]
fn big_struct_page_gets_on_this_page_toc() {
    let mut project = DocProject::new("Big");
    ingest(
        &mut project,
        "Big",
        PackageKind::Project,
        "
        @doc \"Connection pool.\"
        struct Pool
          size: Int

          fn checkout(self, timeout: Int) -> Int ! String
            self.size
          end

          fn release(self) -> Int
            self.size
          end

          fn stats(self) -> Int
            self.size
          end
        end
        ",
    );
    finalize_project(&mut project);

    let big = project.find_package("Big").expect("Big present");
    let pool = big.structs.iter().find(|s| s.name == "Pool").expect("Pool");
    let html = render_struct(pool, big, &project);

    assert!(html.contains("On this page"));
    assert!(html.contains("href=\"#fields\""));
    assert!(html.contains("href=\"#fn-checkout-arity-2\""));
    // The fallible spelling survives into the rendered signature.
    assert!(html.contains("! <span class=\"ty\">String</span>"));
}
