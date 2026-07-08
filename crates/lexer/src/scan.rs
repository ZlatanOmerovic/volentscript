//! The scanner. Rules per ECMA-262 3rd ed. §7 as implemented by avmplus
//! `eval-lex.cpp`; file:line citations refer there.

use diagnostics::{Diagnostic, ErrorCode};
use span::{SourceId, Span};

use crate::token::{Token, TokenKind};

/// Lexes an entire source file into a token stream ending with [`TokenKind::Eof`].
///
/// Errors are collected, not fatal: the scanner recovers (skips the offending
/// character / terminates the literal) so the parser still gets a stream and
/// later diagnostics aren't masked.
pub fn lex(source: SourceId, text: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut scanner = Scanner {
        source,
        text,
        bytes: text.as_bytes(),
        pos: 0,
        newline_before: false,
        tokens: Vec::new(),
        diagnostics: Vec::new(),
    };
    scanner.run();
    (scanner.tokens, scanner.diagnostics)
}

struct Scanner<'a> {
    source: SourceId,
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    /// Set when a line terminator is crossed; consumed by the next token.
    newline_before: bool,
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
}

impl Scanner<'_> {
    fn run(&mut self) {
        loop {
            self.skip_trivia();
            let start = self.pos;
            let Some(c) = self.peek_char() else {
                self.push(TokenKind::Eof, start);
                return;
            };
            match c {
                '(' => self.single(TokenKind::LParen, start),
                ')' => self.single(TokenKind::RParen, start),
                '[' => self.single(TokenKind::LBracket, start),
                ']' => self.single(TokenKind::RBracket, start),
                '{' => self.single(TokenKind::LBrace, start),
                '}' => self.single(TokenKind::RBrace, start),
                ';' => self.single(TokenKind::Semicolon, start),
                ',' => self.single(TokenKind::Comma, start),
                '~' => self.single(TokenKind::BitNot, start),
                '?' => self.single(TokenKind::Question, start),
                '.' => self.dot(start),
                ':' => self.colon(start),
                '=' => self.equals(start),
                '!' => self.bang(start),
                '+' => self.plus(start),
                '-' => self.minus(start),
                '*' => self.op_maybe_assign(start, TokenKind::Star, TokenKind::StarAssign),
                '%' => self.op_maybe_assign(start, TokenKind::Percent, TokenKind::PercentAssign),
                '^' => self.op_maybe_assign(start, TokenKind::BitXor, TokenKind::BitXorAssign),
                // Comments were consumed by skip_trivia. A `/` is a regex
                // literal when the previous significant token cannot end an
                // expression, else division — the standard ES3 heuristic
                // standing in for avmplus's parser-fed T_BreakSlash
                // meta-token (eval-lex.cpp:298); ECMA-262 3rd ed. §7.8.5.
                '/' => {
                    if self.regex_allowed() {
                        self.regex(start)
                    } else {
                        self.op_maybe_assign(start, TokenKind::Slash, TokenKind::SlashAssign)
                    }
                }
                '&' => self.amp(start),
                '|' => self.pipe(start),
                '<' => self.left_angle(start),
                '>' => self.right_angle(start),
                '"' | '\'' => self.string_literal(start, c),
                '0'..='9' => self.number(start),
                c if is_ident_start(c) => self.identifier(start),
                c => {
                    self.bump(c);
                    self.error(
                        ErrorCode::UNEXPECTED_CHAR,
                        format!("unexpected character `{c}`"),
                        start,
                    );
                }
            }
        }
    }

    // --- trivia ---------------------------------------------------------

    fn skip_trivia(&mut self) {
        loop {
            match self.peek_char() {
                Some(c) if is_line_terminator(c) => {
                    self.bump(c);
                    self.newline_before = true;
                }
                Some(c) if is_whitespace(c) => self.bump(c),
                Some('/') if self.peek_byte_at(1) == Some(b'/') => {
                    while let Some(c) = self.peek_char() {
                        if is_line_terminator(c) {
                            break;
                        }
                        self.bump(c);
                    }
                }
                Some('/') if self.peek_byte_at(1) == Some(b'*') => self.block_comment(),
                _ => return,
            }
        }
    }

    fn block_comment(&mut self) {
        let start = self.pos;
        self.pos += 2; // "/*"
        loop {
            match self.peek_char() {
                None => {
                    self.error(
                        ErrorCode::UNTERMINATED_COMMENT,
                        "unterminated block comment",
                        start,
                    );
                    return;
                }
                Some('*') if self.peek_byte_at(1) == Some(b'/') => {
                    self.pos += 2;
                    return;
                }
                Some(c) => {
                    // A multi-line comment counts as a line terminator for
                    // semicolon insertion (ECMA-262 3rd ed. §7.4).
                    if is_line_terminator(c) {
                        self.newline_before = true;
                    }
                    self.bump(c);
                }
            }
        }
    }

    // --- compound operators ----------------------------------------------

    fn dot(&mut self, start: usize) {
        // Order matters: "..." before "..", ".<" (single token, avmplus
        // T_LeftDotAngle eval-lex.cpp:329), ".5" leading-dot float
        // (eval-lex.cpp:333).
        if self.peek_byte_at(1) == Some(b'.') && self.peek_byte_at(2) == Some(b'.') {
            self.pos += 3;
            self.push(TokenKind::Ellipsis, start);
        } else if self.peek_byte_at(1) == Some(b'.') {
            self.pos += 2;
            self.push(TokenKind::DotDot, start);
        } else if self.peek_byte_at(1) == Some(b'<') {
            self.pos += 2;
            self.push(TokenKind::LeftDotAngle, start);
        } else if self.peek_byte_at(1).is_some_and(|b| b.is_ascii_digit()) {
            self.number(start);
        } else {
            self.pos += 1;
            self.push(TokenKind::Dot, start);
        }
    }

    fn colon(&mut self, start: usize) {
        if self.peek_byte_at(1) == Some(b':') {
            self.pos += 2;
            self.push(TokenKind::ColonColon, start);
        } else {
            self.pos += 1;
            self.push(TokenKind::Colon, start);
        }
    }

    fn equals(&mut self, start: usize) {
        if self.starts_with("===") {
            self.pos += 3;
            self.push(TokenKind::StrictEq, start);
        } else if self.starts_with("==") {
            self.pos += 2;
            self.push(TokenKind::Eq, start);
        } else {
            self.pos += 1;
            self.push(TokenKind::Assign, start);
        }
    }

    fn bang(&mut self, start: usize) {
        if self.starts_with("!==") {
            self.pos += 3;
            self.push(TokenKind::StrictNe, start);
        } else if self.starts_with("!=") {
            self.pos += 2;
            self.push(TokenKind::Ne, start);
        } else {
            self.pos += 1;
            self.push(TokenKind::Not, start);
        }
    }

    fn plus(&mut self, start: usize) {
        if self.starts_with("++") {
            self.pos += 2;
            self.push(TokenKind::PlusPlus, start);
        } else if self.starts_with("+=") {
            self.pos += 2;
            self.push(TokenKind::PlusAssign, start);
        } else {
            self.pos += 1;
            self.push(TokenKind::Plus, start);
        }
    }

    fn minus(&mut self, start: usize) {
        if self.starts_with("--") {
            self.pos += 2;
            self.push(TokenKind::MinusMinus, start);
        } else if self.starts_with("-=") {
            self.pos += 2;
            self.push(TokenKind::MinusAssign, start);
        } else {
            self.pos += 1;
            self.push(TokenKind::Minus, start);
        }
    }

    fn op_maybe_assign(&mut self, start: usize, plain: TokenKind, assign: TokenKind) {
        if self.peek_byte_at(1) == Some(b'=') {
            self.pos += 2;
            self.push(assign, start);
        } else {
            self.pos += 1;
            self.push(plain, start);
        }
    }

    fn amp(&mut self, start: usize) {
        // `&&=` is a real AS3 token (avmplus T_LogicalAndAssign,
        // eval-lex.cpp:390).
        if self.starts_with("&&=") {
            self.pos += 3;
            self.push(TokenKind::LogAndAssign, start);
        } else if self.starts_with("&&") {
            self.pos += 2;
            self.push(TokenKind::LogAnd, start);
        } else if self.starts_with("&=") {
            self.pos += 2;
            self.push(TokenKind::BitAndAssign, start);
        } else {
            self.pos += 1;
            self.push(TokenKind::BitAnd, start);
        }
    }

    fn pipe(&mut self, start: usize) {
        // `||=` likewise (avmplus T_LogicalOrAssign, eval-lex.cpp:429).
        if self.starts_with("||=") {
            self.pos += 3;
            self.push(TokenKind::LogOrAssign, start);
        } else if self.starts_with("||") {
            self.pos += 2;
            self.push(TokenKind::LogOr, start);
        } else if self.starts_with("|=") {
            self.pos += 2;
            self.push(TokenKind::BitOrAssign, start);
        } else {
            self.pos += 1;
            self.push(TokenKind::BitOr, start);
        }
    }

    fn left_angle(&mut self, start: usize) {
        if self.starts_with("<<=") {
            self.pos += 3;
            self.push(TokenKind::ShlAssign, start);
        } else if self.starts_with("<<") {
            self.pos += 2;
            self.push(TokenKind::Shl, start);
        } else if self.starts_with("<=") {
            self.pos += 2;
            self.push(TokenKind::Le, start);
        } else {
            self.pos += 1;
            self.push(TokenKind::Lt, start);
        }
    }

    fn right_angle(&mut self, start: usize) {
        // Longest-match here; the parser re-splits `>>`/`>=`/... when a `>`
        // closes a `.<...>` type application (avmplus instead defers via its
        // T_BreakRightAngle meta-token, eval-lex.cpp:164).
        if self.starts_with(">>>=") {
            self.pos += 4;
            self.push(TokenKind::UshrAssign, start);
        } else if self.starts_with(">>>") {
            self.pos += 3;
            self.push(TokenKind::Ushr, start);
        } else if self.starts_with(">>=") {
            self.pos += 3;
            self.push(TokenKind::ShrAssign, start);
        } else if self.starts_with(">>") {
            self.pos += 2;
            self.push(TokenKind::Shr, start);
        } else if self.starts_with(">=") {
            self.pos += 2;
            self.push(TokenKind::Ge, start);
        } else {
            self.pos += 1;
            self.push(TokenKind::Gt, start);
        }
    }

    // --- identifiers & keywords ------------------------------------------

    fn identifier(&mut self, start: usize) {
        while let Some(c) = self.peek_char() {
            if is_ident_continue(c) {
                self.bump(c);
            } else {
                break;
            }
        }
        let text = &self.text[start..self.pos];
        match TokenKind::keyword(text) {
            Some(kw) => self.push(kw, start),
            None => self.push(TokenKind::Ident(text.to_string()), start),
        }
    }

    // --- numbers ----------------------------------------------------------

    fn number(&mut self, start: usize) {
        if self.starts_with("0x") || self.starts_with("0X") {
            self.pos += 2;
            let digits_start = self.pos;
            while self.peek_char().is_some_and(|c| c.is_ascii_hexdigit()) {
                self.pos += 1;
            }
            if self.pos == digits_start {
                self.error(
                    ErrorCode::MALFORMED_NUMBER,
                    "hex literal needs at least one digit",
                    start,
                );
                self.push(TokenKind::Int(0), start);
                return;
            }
            self.check_number_end(start);
            let value =
                u64::from_str_radix(&self.text[digits_start..self.pos], 16).unwrap_or(u64::MAX);
            self.push(classify_integer(value as f64), start);
            return;
        }

        // Decimal: digits, optional fraction, optional exponent (ECMA-262
        // 3rd ed. §7.8.3; avmplus eval-lex.cpp:1148). Octal literals are not
        // supported — avmplus default mode likewise treats leading-0 numbers
        // as decimal.
        let mut is_double = false;
        while self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek_char() == Some('.') && self.peek_byte_at(1) != Some(b'.') {
            // Not consuming `.` before `..`: `0..toString()` keeps working.
            is_double = true;
            self.pos += 1;
            while self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek_char(), Some('e' | 'E')) {
            let mut lookahead = 1;
            if matches!(self.peek_byte_at(1), Some(b'+' | b'-')) {
                lookahead = 2;
            }
            if self
                .peek_byte_at(lookahead)
                .is_some_and(|b| b.is_ascii_digit())
            {
                is_double = true;
                self.pos += lookahead;
                while self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
        }
        self.check_number_end(start);
        let text = &self.text[start..self.pos];
        let value: f64 = text.parse().unwrap_or(f64::NAN);
        if is_double {
            self.push(TokenKind::Number(value), start);
        } else {
            self.push(classify_integer(value), start);
        }
    }

    /// An identifier character directly after a number is an error
    /// (avmplus SYNTAXERR_ILLEGALCHAR_POSTNUMBER, eval-lex.cpp:1271).
    fn check_number_end(&mut self, start: usize) {
        if self.peek_char().is_some_and(is_ident_start) {
            self.error(
                ErrorCode::MALFORMED_NUMBER,
                "identifier characters cannot directly follow a number",
                start,
            );
            while self.peek_char().is_some_and(is_ident_continue) {
                self.pos += 1;
            }
        }
    }

    // --- strings -----------------------------------------------------------

    fn string_literal(&mut self, start: usize, quote: char) {
        self.pos += 1;
        let mut value = String::new();
        loop {
            match self.peek_char() {
                None => {
                    self.error(
                        ErrorCode::UNTERMINATED_STRING,
                        "unterminated string literal",
                        start,
                    );
                    break;
                }
                Some(c) if c == quote => {
                    self.pos += 1;
                    break;
                }
                // Raw line terminator ends (and errors) the literal
                // (avmplus eval-lex.cpp:1492).
                Some(c) if is_line_terminator(c) => {
                    self.error(
                        ErrorCode::UNTERMINATED_STRING,
                        "string literal must not contain a raw line break (use \\n or a \\ line continuation)",
                        start,
                    );
                    break;
                }
                Some('\\') => self.escape_sequence(&mut value),
                Some(c) => {
                    value.push(c);
                    self.bump(c);
                }
            }
        }
        self.push(TokenKind::Str(value), start);
    }

    fn escape_sequence(&mut self, value: &mut String) {
        self.pos += 1; // backslash
        let Some(c) = self.peek_char() else {
            return; // unterminated-string error follows at loop head
        };
        match c {
            // Line continuation: backslash + line terminator produces
            // nothing (avmplus eval-lex.cpp:1515). CRLF counts as one.
            c if is_line_terminator(c) => {
                self.bump(c);
                if c == '\r' && self.peek_char() == Some('\n') {
                    self.pos += 1;
                }
                self.newline_before = true;
            }
            'b' => self.simple_escape(value, '\u{0008}'),
            't' => self.simple_escape(value, '\t'),
            'n' => self.simple_escape(value, '\n'),
            'v' => self.simple_escape(value, '\u{000B}'),
            'f' => self.simple_escape(value, '\u{000C}'),
            'r' => self.simple_escape(value, '\r'),
            '0' => self.simple_escape(value, '\0'),
            // \xHH — exactly two hex digits, else the escape degrades to a
            // literal `x` (avmplus eval-lex.cpp:1565).
            'x' => {
                self.pos += 1;
                match self.hex_digits(2) {
                    Some(code) => value.push(char::from_u32(code).unwrap_or('\u{FFFD}')),
                    None => value.push('x'),
                }
            }
            // \uHHHH or \u{...}; invalid degrades to literal `u`
            // (avmplus eval-lex.cpp:1576).
            'u' => {
                self.pos += 1;
                let code = if self.peek_char() == Some('{') {
                    self.pos += 1;
                    let code = self.hex_digits_until_brace();
                    if self.peek_char() == Some('}') {
                        self.pos += 1;
                    }
                    code
                } else {
                    self.hex_digits(4)
                };
                match code {
                    Some(code) => value.push(char::from_u32(code).unwrap_or('\u{FFFD}')),
                    None => value.push('u'),
                }
            }
            // Any other escaped character is itself — covers \' \" \\
            // (avmplus eval-lex.cpp:1611,1628).
            c => self.simple_escape(value, c),
        }
    }

    fn simple_escape(&mut self, value: &mut String, out: char) {
        value.push(out);
        let c = self.peek_char().expect("caller peeked");
        self.bump(c);
    }

    fn hex_digits(&mut self, n: usize) -> Option<u32> {
        let start = self.pos;
        for _ in 0..n {
            if !self.peek_char().is_some_and(|c| c.is_ascii_hexdigit()) {
                self.pos = start;
                return None;
            }
            self.pos += 1;
        }
        u32::from_str_radix(&self.text[start..self.pos], 16).ok()
    }

    fn hex_digits_until_brace(&mut self) -> Option<u32> {
        let start = self.pos;
        while self.peek_char().is_some_and(|c| c.is_ascii_hexdigit()) {
            self.pos += 1;
        }
        if self.pos == start {
            return None;
        }
        u32::from_str_radix(&self.text[start..self.pos], 16).ok()
    }

    // --- plumbing -----------------------------------------------------------

    fn peek_char(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn peek_byte_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn starts_with(&self, s: &str) -> bool {
        self.bytes[self.pos..].starts_with(s.as_bytes())
    }

    fn bump(&mut self, c: char) {
        self.pos += c.len_utf8();
    }

    fn single(&mut self, kind: TokenKind, start: usize) {
        self.pos += 1;
        self.push(kind, start);
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        let span = Span::new(
            self.source,
            u32::try_from(start).expect("file too large"),
            u32::try_from(self.pos).expect("file too large"),
        );
        self.tokens.push(Token {
            kind,
            span,
            newline_before: std::mem::take(&mut self.newline_before),
        });
    }

    /// Whether a `/` at the current position starts a regex literal:
    /// true when the previous significant token cannot end an expression
    /// (ES3 lexical-grammar disambiguation, ECMA-262 3rd ed. §7.8.5).
    fn regex_allowed(&self) -> bool {
        match self.tokens.last().map(|t| &t.kind) {
            None => true,
            Some(k) => !matches!(
                k,
                TokenKind::Int(_)
                    | TokenKind::UInt(_)
                    | TokenKind::Number(_)
                    | TokenKind::Str(_)
                    | TokenKind::Ident(_)
                    | TokenKind::RegExp(..)
                    | TokenKind::RParen
                    | TokenKind::RBracket
                    | TokenKind::This
                    | TokenKind::Super
                    | TokenKind::True
                    | TokenKind::False
                    | TokenKind::Null
                    | TokenKind::PlusPlus
                    | TokenKind::MinusMinus
            ),
        }
    }

    /// Scans `/pattern/flags` past the opening `/`. A `/` inside a
    /// character class does not terminate; `\` escapes anything; a line
    /// terminator or EOF before the closing `/` is an error (§7.8.5).
    fn regex(&mut self, start: usize) {
        self.pos += 1; // opening '/'
        let body_start = self.pos;
        let mut in_class = false;
        loop {
            match self.peek_char() {
                None | Some('\n') | Some('\r') => {
                    self.error(
                        ErrorCode::UNTERMINATED_REGEX,
                        "unterminated regular expression literal",
                        start,
                    );
                    return;
                }
                Some('\\') => {
                    self.pos += 1;
                    if let Some(c) = self.peek_char()
                        && c != '\n'
                        && c != '\r'
                    {
                        self.bump(c);
                    }
                }
                Some('[') => {
                    in_class = true;
                    self.pos += 1;
                }
                Some(']') => {
                    in_class = false;
                    self.pos += 1;
                }
                Some('/') if !in_class => break,
                Some(c) => self.bump(c),
            }
        }
        let pattern = self.text[body_start..self.pos].to_string();
        self.pos += 1; // closing '/'
        let flags_start = self.pos;
        while let Some(c) = self.peek_char() {
            if c.is_ascii_alphabetic() {
                self.bump(c);
            } else {
                break;
            }
        }
        let flags = self.text[flags_start..self.pos].to_string();
        self.push(TokenKind::RegExp(pattern, flags), start);
    }

    fn error(&mut self, code: ErrorCode, message: impl Into<String>, start: usize) {
        let span = Span::new(self.source, start as u32, self.pos as u32);
        self.diagnostics
            .push(Diagnostic::error(code, message).with_span(span));
    }
}

/// Integer literals classify by magnitude into int/uint/Number
/// (avmplus eval-lex.cpp:1218).
fn classify_integer(value: f64) -> TokenKind {
    if value <= i32::MAX as f64 {
        TokenKind::Int(value as i32)
    } else if value <= u32::MAX as f64 {
        TokenKind::UInt(value as u32)
    } else {
        TokenKind::Number(value)
    }
}

fn is_ident_start(c: char) -> bool {
    // ASCII fast path plus general Unicode letters (ECMA-262 3rd ed. §7.6
    // allows Unicode letters; we use Rust's `is_alphabetic` as the
    // approximation of the Unicode tables avmplus generates).
    c == '_' || c == '$' || c.is_ascii_alphabetic() || (!c.is_ascii() && c.is_alphabetic())
}

fn is_ident_continue(c: char) -> bool {
    is_ident_start(c) || c.is_ascii_digit() || (!c.is_ascii() && c.is_numeric())
}

fn is_whitespace(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\u{000B}' | '\u{000C}' | '\u{00A0}' | '\u{FEFF}'
    ) || (!c.is_ascii() && c.is_whitespace() && !is_line_terminator(c))
}

/// Line terminators per ECMA-262 3rd ed. §7.3: LF, CR, LS, PS.
fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TokenKind::*;

    fn kinds(text: &str) -> Vec<TokenKind> {
        let (tokens, diags) = lex(SourceId(0), text);
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        tokens.into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn keywords_and_identifiers() {
        assert_eq!(
            kinds("var x is as foo each"),
            vec![
                Var,
                Ident("x".into()),
                Is,
                As,
                Ident("foo".into()),
                // contextual, not a keyword (avmplus eval-parse-stmt.cpp:435)
                Ident("each".into()),
                Eof
            ]
        );
    }

    #[test]
    fn compound_operators_including_logical_assign() {
        assert_eq!(
            kinds("a &&= b ||= c >>>= d !== e"),
            vec![
                Ident("a".into()),
                LogAndAssign,
                Ident("b".into()),
                LogOrAssign,
                Ident("c".into()),
                UshrAssign,
                Ident("d".into()),
                StrictNe,
                Ident("e".into()),
                Eof
            ]
        );
    }

    #[test]
    fn dot_family() {
        assert_eq!(
            kinds("a.b ... .. Vector.<int> .5"),
            vec![
                Ident("a".into()),
                Dot,
                Ident("b".into()),
                Ellipsis,
                DotDot,
                Ident("Vector".into()),
                LeftDotAngle,
                Ident("int".into()),
                Gt,
                Number(0.5),
                Eof
            ]
        );
    }

    #[test]
    fn numbers_classify_by_magnitude() {
        // per avmplus eval-lex.cpp:1218
        assert_eq!(
            kinds("42 2147483647 2147483648 4294967295 4294967296 1.5 1e3 0xFF"),
            vec![
                Int(42),
                Int(2147483647),
                UInt(2147483648),
                UInt(4294967295),
                Number(4294967296.0),
                Number(1.5),
                Number(1000.0),
                Int(255),
                Eof
            ]
        );
    }

    #[test]
    fn string_escapes_and_line_continuation() {
        assert_eq!(
            kinds("\"a\\tb\\u0041\\x41\\\n c\""),
            vec![Str("a\tbAA c".into()), Eof]
        );
        // invalid \x degrades to literal x (avmplus eval-lex.cpp:1565)
        assert_eq!(kinds("\"\\xZZ\""), vec![Str("xZZ".into()), Eof]);
    }

    #[test]
    fn newline_before_flag() {
        let (tokens, _) = lex(SourceId(0), "a\nb /* \n */ c d");
        let flags: Vec<bool> = tokens.iter().map(|t| t.newline_before).collect();
        // b: after newline; c: block comment containing newline counts (§7.4)
        assert_eq!(flags, vec![false, true, true, false, false]);
    }

    #[test]
    fn string_errors_recover() {
        let (tokens, diags) = lex(SourceId(0), "\"abc\nvar");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, Some(ErrorCode::UNTERMINATED_STRING));
        assert_eq!(tokens.last().unwrap().kind, Eof);
        assert!(tokens.iter().any(|t| t.kind == Var), "scanner must recover");
    }
}
