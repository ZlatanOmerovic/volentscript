//! Anti-rot suite for `examples/`: every example compiles; the
//! deterministic ones also run with asserted output.

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn runtime_lib() -> PathBuf {
    let mut dir = std::env::current_exe().expect("current_exe");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    dir.join("libruntime.a")
}

fn build(rel: &str, out: &Path) -> PathBuf {
    driver::build(&driver::BuildOptions {
        input: workspace_root().join(rel),
        output: Some(out.to_path_buf()),
        runtime_lib: Some(runtime_lib()),
        opt: driver::OptLevel::default(),
        target: None,
    })
    .unwrap_or_else(|e| panic!("{rel} failed to build:\n{}", e.rendered.join("\n")))
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("vs-ex-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

/// Every example program compiles.
#[test]
fn all_examples_compile() {
    let dir = temp_dir("compile");
    for rel in [
        "examples/life/main.vlt",
        "examples/todo/main.vlt",
        "examples/vgrep/main.vlt",
        "examples/calc/main.vlt",
        "examples/logstats/main.vlt",
        "examples/httpd/main.vlt",
        "examples/mail/smtpd.vlt",
        "examples/mail/send.vlt",
        "examples/chat/server.vlt",
        "examples/chat/client.vlt",
    ] {
        let name = rel.replace('/', "_");
        build(rel, &dir.join(name));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn life_runs() {
    let dir = temp_dir("life");
    let exe = build("examples/life/main.vlt", &dir.join("life"));
    let out = Command::new(&exe).arg("20").output().expect("run");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.starts_with("generation 20:"));
    // The glider stabilizes into a 2x2 block on this grid.
    assert_eq!(text.matches('#').count(), 4);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn calc_repl() {
    use std::io::Write as _;
    let dir = temp_dir("calc");
    let exe = build("examples/calc/main.vlt", &dir.join("calc"));
    let mut child = Command::new(&exe)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"2 + 3 * (4 - 1)\n1/0\nquit\n")
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("\n11\n"), "got: {text}");
    assert!(text.contains("RangeError: division by zero"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vgrep_tree_and_exit_codes() {
    let dir = temp_dir("vgrep");
    let exe = build("examples/vgrep/main.vlt", &dir.join("vgrep"));
    std::fs::create_dir_all(dir.join("t/sub")).expect("mkdir");
    std::fs::write(dir.join("t/a.txt"), "alpha\nTODO one\n").expect("write");
    std::fs::write(dir.join("t/sub/b.txt"), "TODO two\n").expect("write");
    let hit = Command::new(&exe)
        .arg("TODO")
        .arg(dir.join("t"))
        .output()
        .expect("run");
    assert_eq!(hit.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&hit.stdout).matches("TODO").count(),
        2
    );
    let miss = Command::new(&exe)
        .arg("ZZZ")
        .arg(dir.join("t"))
        .output()
        .expect("run");
    assert_eq!(miss.status.code(), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn logstats_report() {
    let dir = temp_dir("logstats");
    let exe = build("examples/logstats/main.vlt", &dir.join("logstats"));
    std::fs::copy(
        workspace_root().join("examples/logstats/sample.log"),
        dir.join("sample.log"),
    )
    .expect("copy");
    let out = Command::new(&exe)
        .arg("sample.log")
        .current_dir(&dir)
        .output()
        .expect("run");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("parsed 9 lines (1 skipped)"), "got: {text}");
    assert!(text.contains("200: 5"));
    assert!(text.contains("3x /index.html"));
    let report = std::fs::read_to_string(dir.join("report.json")).expect("report");
    assert!(report.contains("\"parsed\":9"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn todo_lifecycle() {
    let dir = temp_dir("todo");
    let exe = build("examples/todo/main.vlt", &dir.join("todo"));
    let run = |args: &[&str]| -> String {
        let out = Command::new(&exe)
            .args(args)
            .current_dir(&dir)
            .output()
            .expect("run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    run(&["add", "one"]);
    run(&["add", "two"]);
    assert!(run(&["list"]).contains("2. [ ] two"));
    run(&["done", "1"]);
    assert!(run(&["list"]).contains("1. [x] one"));
    run(&["rm", "2"]);
    assert!(!run(&["list"]).contains("two"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn httpd_serves() {
    use std::io::BufRead as _;
    let dir = temp_dir("httpd");
    let exe = build("examples/httpd/main.vlt", &dir.join("httpd"));
    std::fs::create_dir_all(dir.join("public")).expect("mkdir");
    std::fs::write(dir.join("public/index.html"), "<h1>volent</h1>").expect("write");
    let mut server = Command::new(&exe)
        .args(["0", "public", "2"])
        .current_dir(&dir)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut first = String::new();
    std::io::BufReader::new(server.stdout.as_mut().expect("stdout"))
        .read_line(&mut first)
        .expect("read");
    let port = first.rsplit(':').next().expect("port").trim().to_string();

    let get = |path: &str| -> String {
        use std::io::{Read as _, Write as _};
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port.parse::<u16>().expect("port")))
            .expect("connect");
        write!(s, "GET {path} HTTP/1.0\r\n\r\n").expect("send");
        let mut buf = String::new();
        s.read_to_string(&mut buf).expect("read");
        buf
    };
    assert!(get("/").contains("<h1>volent</h1>"));
    assert!(get("/nope").starts_with("HTTP/1.0 404"));
    let _ = server.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn smtp_delivery() {
    use std::io::BufRead as _;
    let dir = temp_dir("smtp");
    let smtpd = build("examples/mail/smtpd.vlt", &dir.join("smtpd"));
    let send = build("examples/mail/send.vlt", &dir.join("send"));
    let mut server = Command::new(&smtpd)
        .args(["0", "mail", "1"])
        .current_dir(&dir)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut first = String::new();
    std::io::BufReader::new(server.stdout.as_mut().expect("stdout"))
        .read_line(&mut first)
        .expect("read");
    let port = first.rsplit(' ').next().expect("port").trim().to_string();

    let client = Command::new(&send)
        .args([&port, "a@volent.local", "b@volent.local", "hello wire"])
        .output()
        .expect("client");
    assert!(String::from_utf8_lossy(&client.stdout).contains("sent"));
    let _ = server.wait();
    let eml = std::fs::read_to_string(dir.join("mail/1.eml")).expect("delivered");
    assert!(eml.contains("hello wire"));
    let _ = std::fs::remove_dir_all(&dir);
}
