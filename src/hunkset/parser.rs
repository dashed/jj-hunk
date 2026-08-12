use super::ast::{Arg, Expr, PatternKind, StringPattern};
use super::error::HunksetError;

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum TokenKind {
    LParen,
    RParen,
    Pipe,
    Ampersand,
    Tilde,
    Comma,
    DotDot,
    Ident(String),
    Str(String),
    Number(usize),
    Colon,
}

impl TokenKind {
    /// How this token looked in the input.
    ///
    /// Error messages quote the user's own text; `{:?}` on the variant would
    /// show internal names like `RParen` and `Tilde`, which mean nothing to
    /// someone who typed `)` and `~`.
    fn describe(&self) -> String {
        match self {
            TokenKind::LParen => "'('".to_string(),
            TokenKind::RParen => "')'".to_string(),
            TokenKind::Pipe => "'|'".to_string(),
            TokenKind::Ampersand => "'&'".to_string(),
            TokenKind::Tilde => "'~'".to_string(),
            TokenKind::Comma => "','".to_string(),
            TokenKind::DotDot => "'..'".to_string(),
            TokenKind::Colon => "':'".to_string(),
            TokenKind::Ident(name) => format!("'{name}'"),
            TokenKind::Str(value) => format!("string \"{value}\""),
            TokenKind::Number(n) => format!("number {n}"),
        }
    }
}

#[derive(Debug, Clone)]
struct Token {
    kind: TokenKind,
    pos: usize,
}

/// A tokenizer failure that already knows where it happened.
///
/// The position travels with the message rather than being scraped back out
/// of it, so a message is free to mention anything -- including another
/// "position" -- without moving the caret.
#[derive(Debug)]
struct TokenizeError {
    message: String,
    position: usize,
}

struct Tokenizer {
    chars: Vec<char>,
    pos: usize,
}

impl Tokenizer {
    fn new(input: &str) -> Self {
        Self {
            chars: input.chars().collect(),
            pos: 0,
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next_char(&mut self) -> Option<char> {
        let ch = self.chars.get(self.pos).copied();
        if ch.is_some() {
            self.pos += 1;
        }
        ch
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek_char() {
            if ch.is_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn emit(&self, kind: TokenKind, pos: usize) -> Token {
        Token { kind, pos }
    }

    fn error_at(&self, position: usize, message: impl Into<String>) -> TokenizeError {
        TokenizeError { message: message.into(), position }
    }

    fn tokenize(&mut self) -> Result<Vec<Token>, TokenizeError> {
        let mut tokens = Vec::new();

        loop {
            self.skip_whitespace();
            let Some(ch) = self.peek_char() else {
                break;
            };

            let start = self.pos;
            match ch {
                '(' => { self.next_char(); tokens.push(self.emit(TokenKind::LParen, start)); }
                ')' => { self.next_char(); tokens.push(self.emit(TokenKind::RParen, start)); }
                '|' => { self.next_char(); tokens.push(self.emit(TokenKind::Pipe, start)); }
                '&' => { self.next_char(); tokens.push(self.emit(TokenKind::Ampersand, start)); }
                '~' => { self.next_char(); tokens.push(self.emit(TokenKind::Tilde, start)); }
                ',' => { self.next_char(); tokens.push(self.emit(TokenKind::Comma, start)); }
                ':' => { self.next_char(); tokens.push(self.emit(TokenKind::Colon, start)); }
                '.' => {
                    if self.chars.get(self.pos + 1) == Some(&'.') {
                        self.pos += 2;
                        tokens.push(self.emit(TokenKind::DotDot, start));
                    } else {
                        return Err(self.error_at(start, "unexpected '.' -- a range is written '..', as in lines(10..20)"));
                    }
                }
                // `!` is not an operator here. Accepting it silently -- which
                // is what happened while this fell through to the YAML parser
                // -- selected nothing and gave the user no clue why.
                '!' => {
                    return Err(self.error_at(
                        start,
                        "'!' is not an operator -- use '~' for negation ('~x') \
                         and for difference ('x ~ y')",
                    ));
                }
                '"' => { tokens.push(self.read_string(start)?); }
                _ if ch.is_ascii_digit() => { tokens.push(self.read_number(start)?); }
                _ if is_ident_start(ch) => { tokens.push(self.read_ident(start)); }
                _ => {
                    return Err(self.error_at(start, format!("unexpected character '{ch}'")));
                }
            }
        }

        Ok(tokens)
    }

    fn read_string(&mut self, start: usize) -> Result<Token, TokenizeError> {
        self.next_char(); // consume opening quote
        let mut value = String::new();
        loop {
            match self.next_char() {
                Some('"') => return Ok(Token { kind: TokenKind::Str(value), pos: start }),
                Some('\\') => match self.next_char() {
                    Some('n') => value.push('\n'),
                    Some('t') => value.push('\t'),
                    Some('\\') => value.push('\\'),
                    Some('"') => value.push('"'),
                    Some(c) => { value.push('\\'); value.push(c); }
                    // Point at the quote that opened the string, not at the
                    // end of the input: that is the character to fix.
                    None => return Err(self.error_at(start, "unterminated string escape")),
                },
                Some(c) => value.push(c),
                None => return Err(self.error_at(start, "unterminated string literal")),
            }
        }
    }

    fn read_number(&mut self, start: usize) -> Result<Token, TokenizeError> {
        let mut digits = String::new();
        while let Some(ch) = self.peek_char() {
            if ch.is_ascii_digit() {
                digits.push(ch);
                self.next_char();
            } else {
                break;
            }
        }
        let n: usize = digits
            .parse()
            .map_err(|_| self.error_at(start, format!("number '{digits}' is too large")))?;
        Ok(Token { kind: TokenKind::Number(n), pos: start })
    }

    fn read_ident(&mut self, start: usize) -> Token {
        let mut name = String::new();
        while let Some(ch) = self.peek_char() {
            if is_ident_char(ch) {
                name.push(ch);
                self.next_char();
            } else {
                break;
            }
        }
        Token { kind: TokenKind::Ident(name), pos: start }
    }
}

fn is_ident_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

fn is_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// How many levels of `(` / `~` nesting the parser will descend through.
///
/// The descent is recursive, so without a ceiling a long enough run of `(` or
/// `~` exhausts the stack and aborts the process -- a crash, not an error.
///
/// The number is bounded from both sides:
///
/// * From above, by what an expression can plausibly need. Real ones nest two
///   or three deep, and the chaining operators cost nothing here -- `a | b | c`
///   is parsed in a loop, not by recursion -- so only literal parentheses and
///   stacked `~` count against the limit. 128 leaves ample room.
/// * From below, by the stack. A debug build spends roughly 8 KiB per level
///   (measured: at 255 levels a 1 MiB thread overflows and a 2 MiB one does
///   not), so 128 levels peak around 1 MiB. That fits the 2 MiB a spawned
///   thread gets by default, not just the 8 MiB of the main thread -- a limit
///   sized only for the main thread would still crash everywhere else.
///
/// `nesting_at_the_limit_fits_a_default_thread_stack` pins that second bound,
/// so raising this constant without re-measuring fails loudly.
const MAX_DEPTH: usize = 128;

struct Parser {
    input: String,
    /// Length of `input` in characters, not bytes.
    ///
    /// Token positions come from the tokenizer, which indexes characters, so
    /// the "one past the end" position used for end-of-input errors has to be
    /// counted the same way or a caret after any non-ASCII text lands too far
    /// right.
    input_len: usize,
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn new(input: &str, tokens: Vec<Token>) -> Self {
        Self {
            input: input.to_string(),
            input_len: input.chars().count(),
            tokens,
            pos: 0,
            depth: 0,
        }
    }

    /// Descend one level, running `f` there.
    ///
    /// Every recursive path in the parser goes through here, so the counter
    /// cannot drift out of step with the actual stack depth.
    fn nested<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, HunksetError>,
    ) -> Result<T, HunksetError> {
        if self.depth >= MAX_DEPTH {
            return Err(self.error_at(
                self.current_pos(),
                &format!("expression is too deeply nested (limit is {MAX_DEPTH})"),
            ));
        }
        self.depth += 1;
        let result = f(self);
        self.depth -= 1;
        result
    }

    fn peek(&self) -> Option<&TokenKind> {
        self.tokens.get(self.pos).map(|t| &t.kind)
    }

    fn next_token(&mut self) -> Option<Token> {
        let tok = self.tokens.get(self.pos).cloned();
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    fn next_kind(&mut self) -> Option<TokenKind> {
        self.next_token().map(|t| t.kind)
    }

    fn expect(&mut self, expected: &TokenKind) -> Result<(), HunksetError> {
        match self.next_token() {
            Some(tok) if tok.kind == *expected => Ok(()),
            Some(tok) => {
                let msg = format!("expected {}, got {}", expected.describe(), tok.kind.describe());
                Err(self.error_at(tok.pos, &msg))
            }
            None => {
                let msg = format!("expected {}, got end of input", expected.describe());
                Err(self.error_at(self.input_len, &msg))
            }
        }
    }

    fn error_at(&self, pos: usize, msg: &str) -> HunksetError {
        HunksetError::Parse {
            message: msg.to_string(),
            input: self.input.clone(),
            position: pos,
        }
    }

    fn current_pos(&self) -> usize {
        self.tokens.get(self.pos).map(|t| t.pos).unwrap_or(self.input_len)
    }

    fn parse(&mut self) -> Result<Expr, HunksetError> {
        let expr = self.parse_union()?;
        if self.pos < self.tokens.len() {
            let tok = self.tokens[self.pos].clone();
            let mut msg = format!("unexpected {}", tok.kind.describe());
            // The overwhelmingly likely way to arrive here with a `~` is a
            // chain like `a ~ b ~ c`, which this grammar does not group for
            // you. Say so rather than leave the user guessing.
            if tok.kind == TokenKind::Tilde {
                msg.push_str(
                    " -- difference does not chain; add parentheses, \
                     as in '(a ~ b) ~ c'",
                );
            }
            return Err(self.error_at(tok.pos, &msg));
        }
        Ok(expr)
    }

    fn parse_union(&mut self) -> Result<Expr, HunksetError> {
        let mut left = self.parse_intersection()?;
        while self.peek() == Some(&TokenKind::Pipe) {
            self.next_kind();
            let right = self.parse_intersection()?;
            left = Expr::Union(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_intersection(&mut self) -> Result<Expr, HunksetError> {
        let mut left = self.parse_difference()?;
        while self.peek() == Some(&TokenKind::Ampersand) {
            self.next_kind();
            let right = self.parse_difference()?;
            left = Expr::Intersection(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// Difference is intentionally non-associative (`a ~ b ~ c` is a parse
    /// error). Use parentheses: `(a ~ b) ~ c`.
    ///
    /// This is stricter than jj, whose revset parser treats `~` as an ordinary
    /// left-associative infix operator, so `all() ~ none() ~ none()` is
    /// accepted there and means `(all() ~ none()) ~ none()`. The stricter rule
    /// is a deliberate choice for this language: `~` is also the prefix
    /// negation operator, so a bare chain reads ambiguously to a human even
    /// where the grammar is unambiguous, and requiring the parentheses makes
    /// the grouping impossible to misread.
    fn parse_difference(&mut self) -> Result<Expr, HunksetError> {
        let left = self.parse_negation()?;
        if self.peek() == Some(&TokenKind::Tilde) {
            self.next_kind();
            let right = self.parse_negation()?;
            Ok(Expr::Difference(Box::new(left), Box::new(right)))
        } else {
            Ok(left)
        }
    }

    /// Negation is right-recursive: `~~x` is `~(~x)` (double negation).
    fn parse_negation(&mut self) -> Result<Expr, HunksetError> {
        if self.peek() == Some(&TokenKind::Tilde) {
            self.next_kind();
            let inner = self.nested(|p| p.parse_negation())?;
            Ok(Expr::Negation(Box::new(inner)))
        } else {
            self.parse_atom()
        }
    }

    fn parse_atom(&mut self) -> Result<Expr, HunksetError> {
        match self.peek() {
            Some(TokenKind::LParen) => {
                self.next_kind();
                let expr = self.nested(|p| p.parse_union())?;
                self.expect(&TokenKind::RParen)?;
                Ok(expr)
            }
            Some(TokenKind::Ident(_)) => self.parse_function_call(),
            _ => Err(self.error_at(self.current_pos(), "expected function or '('")),
        }
    }

    fn parse_function_call(&mut self) -> Result<Expr, HunksetError> {
        let name = match self.next_kind() {
            Some(TokenKind::Ident(name)) => name,
            _ => return Err(self.error_at(self.current_pos(), "expected function name")),
        };

        if self.peek() != Some(&TokenKind::LParen) {
            return match name.as_str() {
                "all" => Ok(Expr::All),
                "none" => Ok(Expr::None),
                _ => Err(self.error_at(self.current_pos(), &format!("expected '(' after '{}'", name))),
            };
        }

        self.expect(&TokenKind::LParen)?;

        if name == "all" || name == "none" {
            self.expect(&TokenKind::RParen)?;
            return Ok(if name == "all" { Expr::All } else { Expr::None });
        }

        let mut args = Vec::new();
        if self.peek() != Some(&TokenKind::RParen) {
            args.push(self.parse_arg()?);
            while self.peek() == Some(&TokenKind::Comma) {
                self.next_kind();
                args.push(self.parse_arg()?);
            }
        }

        self.expect(&TokenKind::RParen)?;
        Ok(Expr::Function(name, args))
    }

    fn parse_arg(&mut self) -> Result<Arg, HunksetError> {
        match self.peek().cloned() {
            Some(TokenKind::Number(_)) => {
                let n = match self.next_kind() {
                    Some(TokenKind::Number(n)) => n,
                    _ => unreachable!(),
                };
                if self.peek() == Some(&TokenKind::DotDot) {
                    self.next_kind();
                    match self.next_kind() {
                        Some(TokenKind::Number(m)) => Ok(Arg::Range(n, m)),
                        _ => Err(self.error_at(self.current_pos(), "expected number after '..'")),
                    }
                } else {
                    Ok(Arg::Pattern(StringPattern::inferred(
                        PatternKind::Exact,
                        n.to_string(),
                    )))
                }
            }
            Some(TokenKind::Str(_)) => {
                let value = match self.next_kind() {
                    Some(TokenKind::Str(s)) => s,
                    _ => unreachable!(),
                };
                // A bare quoted string carries no explicit kind. Reject a
                // prefix written *inside* the quotes: it would otherwise be
                // matched as literal text and quietly find nothing.
                if let Some((prefix, _)) = value.split_once(':') {
                    if matches!(prefix, "exact" | "substring" | "glob" | "regex") {
                        return Err(self.error_at(
                            self.current_pos(),
                            &format!(
                                "pattern prefix '{prefix}:' must go outside the quotes \
                                 -- write {prefix}:\"...\" instead of \"{prefix}:...\""
                            ),
                        ));
                    }
                }
                Ok(Arg::Pattern(StringPattern::inferred(
                    PatternKind::Substring,
                    value,
                )))
            }
            Some(TokenKind::Ident(_)) => {
                let ident = match self.next_kind() {
                    Some(TokenKind::Ident(s)) => s,
                    _ => unreachable!(),
                };
                if self.peek() == Some(&TokenKind::Colon) {
                    let kind = match ident.as_str() {
                        "exact" => PatternKind::Exact,
                        "substring" => PatternKind::Substring,
                        "glob" => PatternKind::Glob,
                        "regex" => PatternKind::Regex,
                        _ => return Err(self.error_at(self.current_pos(), &format!("unknown pattern kind '{}'", ident))),
                    };
                    self.next_kind(); // consume colon
                    let value = match self.next_kind() {
                        Some(TokenKind::Str(s)) => s,
                        Some(TokenKind::Ident(s)) => s,
                        _ => {
                            return Err(self.error_at(self.current_pos(), &format!("expected string after '{}:'", ident)))
                        }
                    };
                    Ok(Arg::Pattern(StringPattern { kind, value, explicit: true }))
                } else {
                    Ok(Arg::Pattern(StringPattern::inferred(
                        PatternKind::Exact,
                        ident,
                    )))
                }
            }
            _ => Err(self.error_at(self.current_pos(), "expected argument")),
        }
    }
}

/// Parse a hunkset expression string into an AST.
pub fn parse(input: &str) -> Result<Expr, HunksetError> {
    let mut tokenizer = Tokenizer::new(input);
    let tokens = tokenizer.tokenize().map_err(|e| HunksetError::Parse {
        message: e.message,
        input: input.to_string(),
        position: e.position,
    })?;
    if tokens.is_empty() {
        return Err(HunksetError::Parse {
            message: "empty expression".to_string(),
            input: input.to_string(),
            position: 0,
        });
    }
    let mut parser = Parser::new(input, tokens);
    parser.parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(tokens: &[Token]) -> Vec<&TokenKind> {
        tokens.iter().map(|t| &t.kind).collect()
    }

    #[test]
    fn tokenize_simple_expression() {
        let mut t = Tokenizer::new("type(insert) & file(\"src/lib.rs\")");
        let tokens = t.tokenize().unwrap();
        assert_eq!(
            kinds(&tokens),
            vec![
                &TokenKind::Ident("type".into()),
                &TokenKind::LParen,
                &TokenKind::Ident("insert".into()),
                &TokenKind::RParen,
                &TokenKind::Ampersand,
                &TokenKind::Ident("file".into()),
                &TokenKind::LParen,
                &TokenKind::Str("src/lib.rs".into()),
                &TokenKind::RParen,
            ]
        );
    }

    #[test]
    fn tokenize_range() {
        let mut t = Tokenizer::new("lines(10..20)");
        let tokens = t.tokenize().unwrap();
        assert_eq!(
            kinds(&tokens),
            vec![
                &TokenKind::Ident("lines".into()),
                &TokenKind::LParen,
                &TokenKind::Number(10),
                &TokenKind::DotDot,
                &TokenKind::Number(20),
                &TokenKind::RParen,
            ]
        );
    }

    #[test]
    fn tokenize_pattern_prefix() {
        let mut t = Tokenizer::new(r#"added(regex:"fn\s+")"#);
        let tokens = t.tokenize().unwrap();
        assert_eq!(
            kinds(&tokens),
            vec![
                &TokenKind::Ident("added".into()),
                &TokenKind::LParen,
                &TokenKind::Ident("regex".into()),
                &TokenKind::Colon,
                &TokenKind::Str(r"fn\s+".into()),
                &TokenKind::RParen,
            ]
        );
    }

    #[test]
    fn tokenize_string_escapes() {
        let mut t = Tokenizer::new(r#""hello\nworld""#);
        let tokens = t.tokenize().unwrap();
        assert_eq!(tokens.len(), 1);
        match &tokens[0].kind {
            TokenKind::Str(s) => assert_eq!(s, "hello\nworld"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn parse_all_none() {
        assert_eq!(parse("all()").unwrap(), Expr::All);
        assert_eq!(parse("none()").unwrap(), Expr::None);
    }

    #[test]
    fn parse_simple_function() {
        let expr = parse("type(insert)").unwrap();
        assert_eq!(
            expr,
            Expr::Function(
                "type".into(),
                vec![Arg::Pattern(StringPattern {
                    kind: PatternKind::Exact,
                    value: "insert".into(),
                    explicit: false, // bare ident: kind inferred
                })]
            )
        );
    }

    #[test]
    fn parse_union() {
        assert!(matches!(
            parse("type(insert) | type(delete)").unwrap(),
            Expr::Union(_, _)
        ));
    }

    #[test]
    fn parse_intersection() {
        assert!(matches!(
            parse("type(insert) & file(\"src/lib.rs\")").unwrap(),
            Expr::Intersection(_, _)
        ));
    }

    #[test]
    fn parse_negation() {
        assert!(matches!(
            parse("~type(delete)").unwrap(),
            Expr::Negation(_)
        ));
    }

    #[test]
    fn parse_difference() {
        assert!(matches!(
            parse("all() ~ type(delete)").unwrap(),
            Expr::Difference(_, _)
        ));
    }

    #[test]
    fn parse_precedence() {
        // a | b & c should parse as a | (b & c)
        // Matched by reference: `Expr` frees itself iteratively and so cannot
        // be destructured by value.
        match &parse("type(insert) | type(replace) & file(\"x\")").unwrap() {
            Expr::Union(_, right) => assert!(matches!(**right, Expr::Intersection(_, _))),
            other => panic!("expected union, got {:?}", other),
        }
    }

    #[test]
    fn parse_parenthesized() {
        match &parse("(type(insert) | type(replace)) & file(\"x\")").unwrap() {
            Expr::Intersection(left, _) => assert!(matches!(**left, Expr::Union(_, _))),
            other => panic!("expected intersection, got {:?}", other),
        }
    }

    #[test]
    fn parse_range_arg() {
        assert_eq!(
            parse("lines(10..20)").unwrap(),
            Expr::Function("lines".into(), vec![Arg::Range(10, 20)])
        );
    }

    #[test]
    fn parse_pattern_prefix() {
        assert_eq!(
            parse(r#"added(regex:"TODO")"#).unwrap(),
            Expr::Function(
                "added".into(),
                vec![Arg::Pattern(StringPattern {
                    kind: PatternKind::Regex,
                    value: "TODO".into(),
                    explicit: true, // written as regex:"..."
                })]
            )
        );
    }

    #[test]
    fn parse_multiple_args() {
        match &parse(r#"id("hunk-aabb", "hunk-ccdd")"#).unwrap() {
            Expr::Function(name, args) => {
                assert_eq!(name, "id");
                assert_eq!(args.len(), 2);
            }
            other => panic!("expected function, got {:?}", other),
        }
    }

    #[test]
    fn parse_double_negation() {
        match &parse("~~type(delete)").unwrap() {
            Expr::Negation(inner) => assert!(matches!(**inner, Expr::Negation(_))),
            other => panic!("expected double negation, got {:?}", other),
        }
    }

    #[test]
    fn parse_error_shows_caret() {
        let err = parse("type(insert").unwrap_err();
        let display = err.display_with_context();
        assert!(display.contains("type(insert"));
        assert!(display.contains("^"));
    }

    /// The caret must sit under the token the message is about. These are the
    /// positions the pre-existing behaviour got right; they are pinned so the
    /// error-message rewording below cannot quietly move them.
    fn caret_column(input: &str) -> usize {
        let err = parse(input).unwrap_err();
        let display = err.display_with_context();
        let caret_line = display.lines().nth(1).expect("caret line");
        caret_line.find('^').expect("caret")
    }

    #[test]
    fn caret_points_at_the_offending_token() {
        // End of input: one past the last character.
        assert_eq!(caret_column("type(insert"), "type(insert".len());
        // The second `~` in a chained difference.
        assert_eq!(caret_column("all() ~ none() ~ all()"), 15);
        // A stray character sits at its own offset.
        assert_eq!(caret_column("type(insert) @"), 13);
    }

    // -- bug: `!` was silently accepted (via the YAML fallback) ------------

    #[test]
    fn bang_is_rejected_and_points_at_tilde() {
        let err = parse("!import()").unwrap_err();
        let msg = err.display_with_context();
        assert!(msg.contains('!'), "should quote the offending '!': {msg}");
        assert!(
            msg.contains('~'),
            "should name '~' as the operator to use: {msg}"
        );
        assert_eq!(caret_column("!import()"), 0);
    }

    /// Rejecting `!` is the tokenizer's job, so it must not reach inside a
    /// string literal -- `content("no!")` is a perfectly ordinary query.
    #[test]
    fn bang_inside_a_string_literal_is_still_content() {
        assert_eq!(
            parse(r#"content("no!")"#).unwrap(),
            Expr::Function(
                "content".into(),
                vec![Arg::Pattern(StringPattern {
                    kind: PatternKind::Substring,
                    value: "no!".into(),
                    explicit: false,
                })]
            )
        );
        assert!(parse(r#"added(regex:"^\\s*!")"#).is_ok());
    }

    #[test]
    fn bang_is_rejected_in_infix_position() {
        let err = parse("all() ! none()").unwrap_err();
        let msg = err.display_with_context();
        assert!(msg.contains('~'), "should name '~': {msg}");
        assert_eq!(caret_column("all() ! none()"), 6);
    }

    // -- bug: unbounded recursion aborted the process ----------------------

    #[test]
    fn deeply_nested_parens_are_a_parse_error_not_a_crash() {
        let input = format!("{}all(){}", "(".repeat(20_000), ")".repeat(20_000));
        let err = parse(&input).unwrap_err();
        assert!(
            err.to_string().contains("too deeply nested"),
            "expected a nesting error, got: {err}"
        );
    }

    #[test]
    fn deeply_stacked_negations_are_a_parse_error_not_a_crash() {
        let input = format!("{}all()", "~".repeat(100_000));
        let err = parse(&input).unwrap_err();
        assert!(
            err.to_string().contains("too deeply nested"),
            "expected a nesting error, got: {err}"
        );
    }

    /// The boundary itself: `MAX_DEPTH` levels parse, one more is an error.
    /// Pinning both sides keeps an off-by-one in the guard from silently
    /// costing a level (or handing back one it cannot afford).
    #[test]
    fn nesting_is_accepted_up_to_the_limit_and_rejected_past_it() {
        let nest = |depth: usize| format!("{}all(){}", "(".repeat(depth), ")".repeat(depth));
        assert_eq!(parse(&nest(MAX_DEPTH)).unwrap(), Expr::All);
        assert!(parse(&nest(MAX_DEPTH + 1))
            .unwrap_err()
            .to_string()
            .contains("too deeply nested"));

        let tildes = |depth: usize| format!("{}all()", "~".repeat(depth));
        assert!(matches!(
            parse(&tildes(MAX_DEPTH)).unwrap(),
            Expr::Negation(_)
        ));
        assert!(parse(&tildes(MAX_DEPTH + 1))
            .unwrap_err()
            .to_string()
            .contains("too deeply nested"));
    }

    /// The limit is only worth anything if parsing *at* it still fits on a
    /// small stack -- otherwise the guard just moves the crash. 2 MiB is what
    /// a spawned thread gets by default, well under the main thread's 8 MiB.
    ///
    /// This aborts rather than fails if the budget is blown, which is the
    /// point: raising `MAX_DEPTH` past what the stack holds is exactly the
    /// bug the limit exists to prevent.
    #[test]
    fn nesting_at_the_limit_fits_a_default_thread_stack() {
        let parsed = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                // One level past the limit: the parser descends the full
                // `MAX_DEPTH` before refusing, which is the deepest the stack
                // ever has to hold.
                let depth = MAX_DEPTH + 1;
                let parens = format!("{}all(){}", "(".repeat(depth), ")".repeat(depth));
                parse(&parens).is_err()
            })
            .expect("spawn")
            .join()
            .expect("parsing at the depth limit overflowed a 2 MiB stack");
        assert!(parsed);
    }

    #[test]
    fn union_chains_do_not_consume_nesting_depth() {
        // `a | b | c` is parsed iteratively, so a long flat chain must not be
        // mistaken for deep nesting.
        let chain = vec!["all()"; MAX_DEPTH * 4].join(" | ");
        assert!(matches!(parse(&chain).unwrap(), Expr::Union(_, _)));
    }

    // -- bug: parse errors leaked Rust token debug names -------------------

    #[test]
    fn error_names_the_typed_operator_not_the_token_variant() {
        let msg = parse("all() ~ none() ~ all()")
            .unwrap_err()
            .display_with_context();
        assert!(msg.contains('~'), "should show the typed '~': {msg}");
        assert!(
            !msg.contains("Tilde"),
            "leaked the TokenKind variant name: {msg}"
        );
    }

    #[test]
    fn error_names_the_missing_paren_not_the_token_variant() {
        let msg = parse("type(insert").unwrap_err().display_with_context();
        assert!(msg.contains(')'), "should show the missing ')': {msg}");
        assert!(
            !msg.contains("RParen"),
            "leaked the TokenKind variant name: {msg}"
        );
    }

    #[test]
    fn no_error_message_leaks_a_token_variant_name() {
        const VARIANTS: [&str; 11] = [
            "LParen", "RParen", "Pipe", "Ampersand", "Tilde", "Comma", "DotDot", "Ident", "Str",
            "Number", "Colon",
        ];
        for input in [
            "type(insert",
            "all() ~ none() ~ all()",
            "lines(1..)",
            "file(,)",
            "type(insert) type(delete)",
            "|",
            "all() &",
            "added(bogus:\"x\")",
        ] {
            let msg = parse(input).unwrap_err().display_with_context();
            for variant in VARIANTS {
                assert!(
                    !msg.contains(variant),
                    "{input:?} leaked TokenKind::{variant}: {msg}"
                );
            }
        }
    }

    #[test]
    fn chained_difference_error_explains_the_fix() {
        let msg = parse("all() ~ none() ~ all()")
            .unwrap_err()
            .display_with_context();
        assert!(
            msg.contains("parenthes"),
            "should point at parentheses as the fix: {msg}"
        );
    }

    // -- bare `all` / `none` reach the parser ------------------------------

    #[test]
    fn parse_bare_all_and_none() {
        assert_eq!(parse("all").unwrap(), Expr::All);
        assert_eq!(parse("none").unwrap(), Expr::None);
        assert_eq!(parse("(all)").unwrap(), Expr::All);
        assert!(matches!(parse("all | none").unwrap(), Expr::Union(_, _)));
    }
}
