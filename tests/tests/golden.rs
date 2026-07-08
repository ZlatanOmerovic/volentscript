//! Golden tests: compile each `programs/*.as` to a native executable, run
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
    let program = root.join("tests/programs").join(format!("{name}.as"));
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
