//! Sema snapshot tests (SPECS §10): the P2 milestone is type errors with
//! correct spans/codes on a corpus, plus a stable typed-AST dump for the
//! positive corpus. Update with `UPDATE_EXPECT=1 cargo test -p sema`.

use expect_test::expect_file;
use span::SourceMap;

fn run(name: &str, text: &str) -> String {
    let mut sources = SourceMap::new();
    let file = sources.add(name, text);
    let program = match parser::parse(&sources, file) {
        Ok(p) => p,
        Err(diags) => {
            return diags
                .iter()
                .map(|d| d.render_full(&sources))
                .collect::<Vec<_>>()
                .join("\n");
        }
    };
    let outcome = sema::check(&program);
    let mut out = String::new();
    for d in &outcome.diagnostics {
        out.push_str(&d.render_full(&sources));
        out.push('\n');
    }
    if let Some(typed) = &outcome.program {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&sema::dump(typed));
    }
    out
}

/// P2 milestone (positive): the core corpus checks clean and its typed dump
/// (with inserted coercions) is stable.
#[test]
fn typing_snapshot() {
    let text = include_str!("programs/typing.as");
    expect_file!["programs/typing.tast"].assert_eq(&run("typing.as", text));
}

/// P2 milestone (negative): every semantic error carries the right span and
/// stable code.
#[test]
fn type_errors_snapshot() {
    let text = include_str!("programs/errors.as");
    expect_file!["programs/errors.diag"].assert_eq(&run("errors.as", text));
}

#[test]
fn null_into_reference_ok_into_machine_err() {
    // null → String legal; null → int illegal (machine types can't hold
    // null — avmplus Verifier.cpp:1604).
    assert!(!run("a.as", "var s:String = null;").contains("error"));
    assert!(run("b.as", "var i:int = null;").contains("E0302"));
}

#[test]
fn as_with_machine_type_yields_any() {
    // `x as int` is statically `*` (Verifier.cpp:1601-1605).
    let dump = run("c.as", "var x:* = 1; var y:* = x as int;");
    assert!(dump.contains("[*] As int"), "{dump}");
}

#[test]
fn closures_gated_until_p6() {
    let out = run(
        "d.as",
        "function outer():void { var x:int = 1; var f:Function = function():int { return x; }; }",
    );
    assert!(out.contains("Phase 6"), "{out}");
}

/// P4 negative corpus: override enforcement, final, conformance,
/// hierarchy cycles, access control.
#[test]
fn oop_errors_snapshot() {
    let text = include_str!("programs/oop_errors.as");
    expect_file!["programs/oop_errors.diag"].assert_eq(&run("oop_errors.as", text));
}
