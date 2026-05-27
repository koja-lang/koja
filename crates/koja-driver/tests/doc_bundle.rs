//! Smoke tests for `koja doc` and `koja doc --project-only`.
//!
//! Spins the compiled `koja` binary against a tiny fixture
//! project and asserts the on-disk doc tree: a root
//! `index.html`, per-package subdirs (project + bundled stdlib
//! packages) with their own `index.html`, plus the shared
//! `style.css` / `search.js` / `search-index.json` assets at
//! the root. The `--project-only` variant repeats the run and
//! asserts the stdlib subdirs are absent.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn koja_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_koja"))
}

fn write_fixture_project(root: &Path) {
    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        root.join("koja.toml"),
        "[project]\nentry = \"main\"\nname = \"MyApp\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        src_dir.join("main.koja"),
        "@doc \"A widget.\"\nstruct Widget\n  count: Int\nend\n\nfn main\n  0\nend\n",
    )
    .unwrap();
}

fn run_doc(cwd: &Path, args: &[&str]) {
    let output = Command::new(koja_bin())
        .arg("doc")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("failed to run koja doc");
    assert!(
        output.status.success(),
        "koja doc {:?} failed (stderr={})",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn doc_bundle_emits_assets_and_stdlib_packages() {
    let tmp = tempdir();
    write_fixture_project(&tmp);
    run_doc(&tmp, &["-o", "doc"]);

    let doc = tmp.join("doc");
    assert!(doc.join("index.html").is_file(), "doc/index.html missing");
    assert!(doc.join("style.css").is_file(), "doc/style.css missing");
    assert!(doc.join("search.js").is_file(), "doc/search.js missing");
    assert!(
        doc.join("search-index.json").is_file(),
        "doc/search-index.json missing"
    );

    let myapp_index = doc.join("MyApp").join("index.html");
    assert!(myapp_index.is_file(), "MyApp/index.html missing");
    assert!(
        doc.join("MyApp").join("Widget.html").is_file(),
        "Widget.html missing"
    );

    let global_index = doc.join("Global").join("index.html");
    assert!(
        global_index.is_file(),
        "stdlib package Global should be bundled by default"
    );

    let search_json = fs::read_to_string(doc.join("search-index.json")).unwrap();
    assert!(search_json.contains("\"pkg\":\"MyApp\""));
    assert!(search_json.contains("\"name\":\"Widget\""));
}

#[test]
fn doc_skips_stdlib_overlap_with_project_name() {
    let tmp = tempdir();
    let src_dir = tmp.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        tmp.join("koja.toml"),
        "[project]\nentry = \"main\"\nname = \"Net\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        src_dir.join("net.koja"),
        "@doc \"Local Net override.\"\nstruct LocalOnly\n  count: Int\nend\n",
    )
    .unwrap();

    run_doc(&tmp, &["-o", "doc"]);

    let search_json = fs::read_to_string(tmp.join("doc").join("search-index.json")).unwrap();
    let local_hits = search_json.matches("\"name\":\"LocalOnly\"").count();
    assert_eq!(
        local_hits, 1,
        "LocalOnly should appear once in the search index, got {local_hits}: {search_json}"
    );
    let net_ipaddress_hits = search_json.matches("\"name\":\"IPAddress\"").count();
    assert_eq!(
        net_ipaddress_hits, 0,
        "stdlib Net should be skipped when project itself is named Net"
    );
}

#[test]
fn doc_serve_responds_with_correct_content_types() {
    let tmp = tempdir();
    write_fixture_project(&tmp);
    run_doc(&tmp, &["--project-only", "-o", "doc"]);

    let port = pick_free_port();
    let mut child = spawn_serve(&tmp, port);

    let result = (|| -> Result<(), String> {
        wait_for_port(port)?;

        let (status, headers, body) = http_get(port, "/index.html")?;
        assert_eq!(status, 200, "GET /index.html status: body={body}");
        assert!(
            headers.to_lowercase().contains("content-type: text/html"),
            "index.html Content-Type missing: headers={headers}"
        );
        assert!(body.contains("MyApp"), "index body: {body}");

        let (status, headers, _) = http_get(port, "/search-index.json")?;
        assert_eq!(status, 200);
        assert!(
            headers
                .to_lowercase()
                .contains("content-type: application/json"),
            "search-index.json Content-Type missing: headers={headers}"
        );

        let (status, headers, _) = http_get(port, "/search.js")?;
        assert_eq!(status, 200);
        assert!(
            headers.to_lowercase().contains("application/javascript"),
            "search.js Content-Type missing: headers={headers}"
        );

        let (status, ..) = http_get(port, "/")?;
        assert_eq!(status, 200, "directory should resolve to index.html");

        let (status, ..) = http_get(port, "/does-not-exist.html")?;
        assert_eq!(status, 404);

        let (status, ..) = http_get(port, "/../etc/passwd")?;
        assert_eq!(status, 404, "path traversal must be rejected");

        Ok(())
    })();

    let _ = child.kill();
    let _ = child.wait();
    result.unwrap();
}

fn spawn_serve(cwd: &Path, port: u16) -> Child {
    Command::new(koja_bin())
        .arg("doc")
        .arg("serve")
        .arg("--no-rebuild")
        .arg("--port")
        .arg(port.to_string())
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn koja doc serve")
}

fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

fn wait_for_port(port: u16) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            Duration::from_millis(200),
        )
        .is_ok()
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("server didn't bind 127.0.0.1:{port} within 5s"))
}

fn http_get(port: u16, path: &str) -> Result<(u16, String, String), String> {
    let mut stream =
        TcpStream::connect(format!("127.0.0.1:{port}")).map_err(|e| format!("connect: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .write_all(format!("GET {path} HTTP/1.0\r\n\r\n").as_bytes())
        .map_err(|e| format!("write: {e}"))?;

    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .map_err(|e| format!("read: {e}"))?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let split = text.find("\r\n\r\n").ok_or("no header/body split")?;
    let (head, body) = text.split_at(split);
    let body = &body[4..];
    let status_line = head.lines().next().ok_or("empty response")?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .ok_or("no status code")?
        .parse()
        .map_err(|_| "non-numeric status")?;
    Ok((status, head.to_string(), body.to_string()))
}

#[test]
fn doc_project_only_skips_stdlib() {
    let tmp = tempdir();
    write_fixture_project(&tmp);
    run_doc(&tmp, &["--project-only", "-o", "doc"]);

    let doc = tmp.join("doc");
    assert!(doc.join("index.html").is_file());
    assert!(
        doc.join("MyApp").join("Widget.html").is_file(),
        "project Widget page should still exist"
    );
    assert!(
        !doc.join("Global").exists(),
        "Global stdlib package should be absent with --project-only"
    );
    assert!(
        !doc.join("Crypto").exists(),
        "Crypto stdlib package should be absent with --project-only"
    );
}

/// Create a unique temporary directory and return its path. Uses
/// the test name + process id + nanoseconds so parallel runs
/// don't collide. We clean up best-effort via [`Drop`] on the
/// returned [`TempDir`] guard.
fn tempdir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "koja-doc-test-{}-{}-{}",
        std::process::id(),
        nanos,
        seq
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}
