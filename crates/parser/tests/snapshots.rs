//! AST snapshot tests (SPECS §10): parse a program, compare the tree dump.
//! Update snapshots with `UPDATE_EXPECT=1 cargo test -p parser`.

use expect_test::expect_file;
use span::SourceMap;

fn parse_dump(name: &str, text: &str) -> String {
    let mut sources = SourceMap::new();
    let file = sources.add(name, text);
    match parser::parse(&sources, file) {
        Ok(program) => ast::dump(&program),
        Err(diags) => diags
            .iter()
            .map(|d| d.render_full(&sources))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// The P1 milestone: the core-subset corpus parses to a stable AST snapshot.
#[test]
fn core_program_snapshot() {
    let text = include_str!("programs/core.as");
    expect_file!["programs/core.ast"].assert_eq(&parse_dump("core.as", text));
}

/// Negative snapshot: diagnostics carry correct spans and carets.
#[test]
fn syntax_errors_snapshot() {
    let text = include_str!("programs/errors.as");
    expect_file!["programs/errors.diag"].assert_eq(&parse_dump("errors.as", text));
}

#[test]
fn asi_restricted_productions() {
    // `return` + newline returns undefined; the dangling expression becomes
    // its own statement (ECMA-262 3rd ed. §7.9.1).
    let dump = parse_dump("asi.as", "function f() {\n    return\n    1 + 2\n}\n");
    let ret = dump.find("Return").expect("has return");
    let binary = dump.find("Binary Add").expect("has expr stmt");
    assert!(
        ret < binary,
        "expression must not attach to return:\n{dump}"
    );

    // Postfix ++ must not attach across a line break: `a\n++b` is
    // `a; ++b` (avmplus eval-parse-expr.cpp:549).
    let dump = parse_dump("asi2.as", "a\n++b\n");
    assert!(
        dump.contains("Unary PreInc"),
        "++ must parse as prefix on b:\n{dump}"
    );
    assert!(
        !dump.contains("Postfix"),
        "no postfix across newline:\n{dump}"
    );
}

#[test]
fn nested_type_close_splits_shift_tokens() {
    let dump = parse_dump("v.as", "var m:Vector.<Vector.<Vector.<uint>>>;");
    assert!(
        dump.contains("type=Vector.<Vector.<Vector.<uint>>>"),
        "nested .<> must close by splitting >> tokens:\n{dump}"
    );
}

#[test]
fn nullable_type_suffix() {
    let dump = parse_dump("n.as", "var s:String? = null;");
    assert!(dump.contains("type=String?"), "{dump}");
}
