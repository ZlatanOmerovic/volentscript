//! Golden tests: compile each `programs/*.vlt` to a native executable, run
//! it, compare stdout against `programs/*.out` (exit code must be 0).

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    // tests/ crate dir -> workspace root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn runtime_lib() -> PathBuf {
    // The harness runs from target/<profile>/deps; libruntime.a is built by
    // the workspace into target/<profile>/.
    let mut dir = std::env::current_exe().expect("current_exe");
    dir.pop(); // test binary
    if dir.ends_with("deps") {
        dir.pop();
    }
    let lib = dir.join("libruntime.a");
    assert!(
        lib.exists(),
        "libruntime.a not found at {} — build the workspace first",
        lib.display()
    );
    lib
}

fn run_golden(name: &str) {
    let root = workspace_root();
    let program = root.join("tests/programs").join(format!("{name}.vlt"));
    let expected_path = root.join("tests/programs").join(format!("{name}.out"));
    let expected = std::fs::read_to_string(&expected_path)
        .unwrap_or_else(|e| panic!("missing {}: {e}", expected_path.display()));

    let out_dir = std::env::temp_dir().join(format!("vs-golden-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join(name);

    let built = driver::build(&driver::BuildOptions {
        input: program,
        output: Some(exe.clone()),
        runtime_lib: Some(runtime_lib()),
        opt: driver::OptLevel::default(),
        target: None,
    })
    .unwrap_or_else(|e| panic!("build failed:\n{}", e.rendered.join("\n")));

    let output = Command::new(&built).output().expect("run compiled binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "non-zero exit; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout, expected, "stdout mismatch for {name}");
    let _ = std::fs::remove_dir_all(&out_dir);
}

/// The Phase 3 milestone (SPECS §10: "the first golden test").
#[test]
fn hello() {
    run_golden("hello");
}

#[test]
fn functions_and_recursion() {
    run_golden("fib");
}

#[test]
fn numeric_semantics() {
    run_golden("numbers");
}

#[test]
fn strings_and_methods() {
    run_golden("strings");
}

#[test]
fn control_flow() {
    run_golden("control");
}

#[test]
fn dynamic_any() {
    run_golden("any");
}

/// The Phase 4 milestone: polymorphic OOP — classes, interfaces,
/// inheritance, override dispatch, accessors, statics, super, toString.
#[test]
fn oop_polymorphism() {
    run_golden("oop");
}

/// The Phase 5 milestone: generic collections — monomorphized reified
/// Box.<T>, Vector.<T>, Array, split, rest params.
#[test]
fn generic_collections() {
    run_golden("generics");
}

/// The Phase 6 milestone: closures + exceptions — captured state, bound
/// methods, call/apply, comparator sort, for each, typed catch dispatch,
/// runtime errors as catchable objects, finally on every path.
#[test]
fn closures_and_exceptions() {
    run_golden("closures");
}

/// P23: unboxed numeric Vector.<Number>/<int>/<uint> storage with inlined
/// element access — inline read/write, append via runtime, RangeError on
/// the fast path, join/pop/reverse/indexOf, and the boxed String fallback.
#[test]
fn vectors_unboxed() {
    run_golden("vectors");
}

/// P24: bounds-check elimination via loop versioning — fast (unchecked) and
/// slow (checked) paths must agree, and an out-of-range counted loop must
/// still raise RangeError through the guarded slow path.
#[test]
fn bounds_check_elimination() {
    run_golden("bce");
}

/// P25: module-level `const`s (including folded const-expressions and string
/// consts) inline into functions instead of closure-converting them, while a
/// mutable top-level `var` still captures and works.
#[test]
fn module_const_inlining() {
    run_golden("modconst");
}

/// P26: UTF-16-native string ops (split/replace/case/join) match ES on the
/// edge cases a UTF-8 round-trip mishandles — multi-char and empty-separator
/// splits, first-match/empty-search replace, non-ASCII case mapping, and
/// surrogate-pair round-trips.
#[test]
fn strings_utf16_native() {
    run_golden("strutf16");
}

/// The Phase 7 milestone (SPECS §11): a real CLI tool — args, File I/O,
/// dynamic objects, JSON round trip, Math, callbacks, exit codes.
#[test]
fn cli_tool() {
    let root = workspace_root();
    let program = root.join("tests/programs/wordfreq.vlt");
    let out_dir = std::env::temp_dir().join(format!("vs-cli-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join("wordfreq");
    driver::build(&driver::BuildOptions {
        input: program,
        output: Some(exe.clone()),
        runtime_lib: Some(runtime_lib()),
        opt: driver::OptLevel::default(),
        target: None,
    })
    .unwrap_or_else(|e| panic!("build failed:\n{}", e.rendered.join("\n")));
    std::fs::write(
        out_dir.join("input.txt"),
        "the quick brown fox jumps over the lazy dog, the fox laughs\n",
    )
    .expect("fixture");

    // Usage path: exit 2.
    let usage = Command::new(&exe)
        .current_dir(&out_dir)
        .output()
        .expect("run");
    assert_eq!(usage.status.code(), Some(2));

    // Real run: relative paths keep the output stable.
    let output = Command::new(&exe)
        .current_dir(&out_dir)
        .args(["input.txt", "report.json"])
        .output()
        .expect("run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        stdout,
        "wrote report.json\ntotal: 12 unique: 9\nthe = 3\nfox = 2\nquick = 1\nsqrt(unique) ~ 3\n"
    );
    let json = std::fs::read_to_string(out_dir.join("report.json")).expect("report");
    assert_eq!(
        json,
        "{\"file\":\"input.txt\",\"total\":12,\"unique\":9,\"top\":[{\"word\":\"the\",\"count\":3},{\"word\":\"fox\",\"count\":2},{\"word\":\"quick\",\"count\":1}]}"
    );
    let _ = std::fs::remove_dir_all(&out_dir);
}

/// The final golden test (SPECS §10): `tests/showcase.vlt` exercises the
/// whole language surface; its expected stdout lives in the comment block
/// at the bottom of the source file itself.
#[test]
fn showcase() {
    let root = workspace_root();
    let program = root.join("tests/showcase.vlt");
    let source = std::fs::read_to_string(&program).expect("showcase source");

    // Extract the expected block: after the "EXPECTED STDOUT" banner line
    // and its explanatory lines, up to the closing dashed line.
    let mut lines = source
        .lines()
        .skip_while(|l| !l.contains("EXPECTED STDOUT"));
    lines.next(); // banner
    let expected: String = lines
        .skip_while(|l| !l.is_empty()) // rest of the explanation
        .skip(1) // the blank separator itself
        .take_while(|l| !l.starts_with("---"))
        .flat_map(|l| [l, "\n"])
        .collect();
    assert!(!expected.is_empty(), "expected-stdout block not found");

    let out_dir = std::env::temp_dir().join(format!("vs-golden-showcase-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join("showcase");
    let built = driver::build(&driver::BuildOptions {
        input: program,
        output: Some(exe.clone()),
        runtime_lib: Some(runtime_lib()),
        opt: driver::OptLevel::default(),
        target: None,
    })
    .unwrap_or_else(|e| panic!("build failed:\n{}", e.rendered.join("\n")));

    let output = Command::new(&built).output().expect("run compiled binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "non-zero exit; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout, expected, "showcase stdout mismatch");
    let _ = std::fs::remove_dir_all(&out_dir);
}

/// The Phase 9 milestone (SPECS §7): the collector keeps a churn-heavy
/// program's live set bounded and survivors intact.
#[test]
fn gc_churn() {
    run_golden("gcchurn");
}

/// The Phase 10 milestone (SPECS §6): RegExp literals, methods, and the
/// String regex integration.
#[test]
fn regex() {
    run_golden("regex");
}

/// The Phase 11 milestone (SPECS §6): Date instances — constructors,
/// UTC/local getters, setTime, avmplus string forms. All assertions are
/// timezone-independent.
#[test]
fn date() {
    run_golden("date");
}

/// The Phase 12 milestone (SPECS §5): static custom namespaces —
/// declarations (URI identity), namespaced members, qualified access,
/// virtual dispatch through namespaced overrides, `use namespace`.
#[test]
fn namespaces() {
    run_golden("namespaces");
}

/// The Phase 14 milestone (CLAUDE.md §3): cross-compile to Linux and run
/// the showcase in a container. Needs zig, docker, and the target's
/// runtime staticlib, so it is ignored by default:
///   rustup target add aarch64-unknown-linux-gnu
///   cargo build -p runtime --target aarch64-unknown-linux-gnu --release
///   cargo test -p e2e --test golden cross_linux -- --ignored
#[test]
#[ignore = "needs zig + docker + cross runtime (see doc comment)"]
fn cross_linux() {
    let root = workspace_root();
    let out_dir = std::env::temp_dir().join(format!("vs-cross-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join("showcase-linux");
    driver::build(&driver::BuildOptions {
        input: root.join("tests/showcase.vlt"),
        output: Some(exe.clone()),
        runtime_lib: Some(root.join("target/aarch64-unknown-linux-gnu/release/libruntime.a")),
        opt: driver::OptLevel::default(),
        target: Some("aarch64-unknown-linux-gnu".to_string()),
    })
    .unwrap_or_else(|e| panic!("cross build failed:\n{}", e.rendered.join("\n")));

    let output = Command::new("docker")
        .args(["run", "--rm", "--platform", "linux/arm64", "-v"])
        .arg(format!("{}:/w", out_dir.display()))
        .args(["debian:stable-slim", "/w/showcase-linux"])
        .output()
        .expect("docker run");
    assert_eq!(output.status.code(), Some(0));

    let source = std::fs::read_to_string(root.join("tests/showcase.vlt")).expect("source");
    let mut lines = source
        .lines()
        .skip_while(|l| !l.contains("EXPECTED STDOUT"));
    lines.next();
    let expected: String = lines
        .skip_while(|l| !l.is_empty())
        .skip(1)
        .take_while(|l| !l.starts_with("---"))
        .flat_map(|l| [l, "\n"])
        .collect();
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
    let _ = std::fs::remove_dir_all(&out_dir);
}

/// The Phase 15 milestone (SPECS §6 I/O): a TCP echo server and client
/// compiled to native binaries talking over loopback — ephemeral bind,
/// localPort, accept, readLine/write, EOF handling, close.
#[test]
fn sockets_echo() {
    let root = workspace_root();
    let out_dir = std::env::temp_dir().join(format!("vs-sock-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let mut exes = Vec::new();
    for name in ["echo_server", "echo_client"] {
        let exe = out_dir.join(name);
        driver::build(&driver::BuildOptions {
            input: root.join(format!("tests/programs/{name}.vlt")),
            output: Some(exe.clone()),
            runtime_lib: Some(runtime_lib()),
            opt: driver::OptLevel::default(),
            target: None,
        })
        .unwrap_or_else(|e| panic!("build failed:\n{}", e.rendered.join("\n")));
        exes.push(exe);
    }

    let mut server = Command::new(&exes[0])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn server");
    // First line announces the ephemeral port.
    let mut port_line = String::new();
    {
        use std::io::BufRead as _;
        let stdout = server.stdout.as_mut().expect("server stdout");
        std::io::BufReader::new(stdout)
            .read_line(&mut port_line)
            .expect("port line");
    }
    let port = port_line.trim().strip_prefix("PORT ").expect("PORT prefix");

    let client = Command::new(&exes[1]).arg(port).output().expect("client");
    assert_eq!(
        String::from_utf8_lossy(&client.stdout),
        "got: HELLO SOCKETS\ngot: SECOND LINE\ngot: bye\nclient done\n"
    );
    assert_eq!(client.status.code(), Some(0));
    let server_out = server.wait_with_output().expect("server exit");
    assert_eq!(String::from_utf8_lossy(&server_out.stdout), "server done\n");
    assert_eq!(server_out.status.code(), Some(0));
    let _ = std::fs::remove_dir_all(&out_dir);
}

/// The Phase 16 milestone (SPECS §5): first-class Namespace values —
/// URI-interned identity, runtime-computed qualification through the
/// class reflection tables, virtual dispatch, bound methods,
/// ReferenceError on missing members.
#[test]
fn namespace_values() {
    run_golden("nsvalues");
}

/// P18: File IO expansion — directory lifecycle, metadata, copy/rename/
/// append/remove, sorted listing (self-contained scratch dir).
#[test]
fn fileio() {
    run_golden("fileio");
}

/// P18: System.readLine() consumes stdin line-by-line until EOF.
#[test]
fn stdin_lines() {
    let root = workspace_root();
    let out_dir = std::env::temp_dir().join(format!("vs-stdin-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join("stdin_upper");
    driver::build(&driver::BuildOptions {
        input: root.join("tests/programs/stdin_upper.vlt"),
        output: Some(exe.clone()),
        runtime_lib: Some(runtime_lib()),
        opt: driver::OptLevel::default(),
        target: None,
    })
    .unwrap_or_else(|e| panic!("build failed:\n{}", e.rendered.join("\n")));

    use std::io::Write as _;
    let mut child = Command::new(&exe)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"hello\nfile io\n")
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "HELLO\nFILE IO\neof\n"
    );
    assert_eq!(out.status.code(), Some(0));
    let _ = std::fs::remove_dir_all(&out_dir);
}
