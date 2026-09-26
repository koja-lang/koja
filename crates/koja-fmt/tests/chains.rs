mod common;

use common::*;

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
    // An assigned chain breaks after `=` and lines its links up with
    // the root, like an assigned pipe in Elixir.
    assert_fmt(
        r#"
            fn build -> String
              sb = StringBuilder.new().add("GET").add(" ").add("/index.html").add(" HTTP/1.1\r\n").add("Host: ").add("example.com").add("\r\n")
              sb.build()
            end
        "#,
        r#"
            fn build -> String
              sb =
                StringBuilder.new()
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
fn returned_chain_hangs_its_links() {
    // Outside an assignment the links keep the 2 space hang.
    assert_fmt(
        r#"
            fn build -> String
              StringBuilder.new().add("GET").add(" ").add("/index.html").add(" HTTP/1.1\r\n").add("Host: ").add("example.com").build()
            end
        "#,
        r#"
            fn build -> String
              StringBuilder.new()
                .add("GET")
                .add(" ")
                .add("/index.html")
                .add(" HTTP/1.1\r\n")
                .add("Host: ")
                .add("example.com")
                .build()
            end
        "#,
    );
}

#[test]
fn assigned_chain_with_broken_anchor_keeps_links_flush() {
    // The anchor's own argument list breaks, and the closing paren
    // sits flush with the links that follow.
    assert_fmt(
        r#"
            fn f(settings: Settings) -> DbConfig
              db_config = DbConfig.new(settings.database.host, settings.database.port, settings.database.user, settings.database.name).with_password(settings.database.password).with_statement_cache_size(settings.database.statement_cache)
              db_config
            end
        "#,
        r#"
            fn f(settings: Settings) -> DbConfig
              db_config =
                DbConfig.new(
                  settings.database.host,
                  settings.database.port,
                  settings.database.user,
                  settings.database.name,
                )
                .with_password(settings.database.password)
                .with_statement_cache_size(settings.database.statement_cache)
              db_config
            end
        "#,
    );
}

#[test]
fn assigned_chain_that_fits_after_equals_stays_whole() {
    assert_fmt(
        r#"
            fn f(settings: Settings) -> String
              connection_string = settings.database_url.trim().downcase().replace("postgresql", "postgres")
              connection_string
            end
        "#,
        r#"
            fn f(settings: Settings) -> String
              connection_string =
                settings.database_url.trim().downcase().replace("postgresql", "postgres")
              connection_string
            end
        "#,
    );
}

#[test]
fn assigned_single_continuation_keeps_the_hug() {
    assert_fmt(
        r#"
            fn f(settings: Config) -> DbConfig
              config = DbConfig.new(settings.db_host, settings.db_port, settings.db_user, settings.db_name).with_password(settings.db_password)
              config
            end
        "#,
        r#"
            fn f(settings: Config) -> DbConfig
              config = DbConfig.new(
                settings.db_host,
                settings.db_port,
                settings.db_user,
                settings.db_name,
              ).with_password(settings.db_password)
              config
            end
        "#,
    );
}

#[test]
fn compound_assigned_chain_breaks_after_operator() {
    assert_fmt_script(
        r#"
            total += prices.map(price -> price.cents()).filter(cents -> cents > 0).sum().clamp(0, 1000000)
        "#,
        r#"
            total +=
              prices.map(price -> price.cents())
              .filter(cents -> cents > 0)
              .sum()
              .clamp(0, 1000000)
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
              sb =
                StringBuilder.new()
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
