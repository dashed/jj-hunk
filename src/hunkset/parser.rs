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

#[derive(Debug, Clone)]
struct Token {
    kind: TokenKind,
    pos: usize,
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

    fn tokenize(&mut self) -> Result<Vec<Token>, String> {
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
                        return Err(format!("unexpected '.' at position {}", self.pos));
                    }
                }
                '"' => { tokens.push(self.read_string(start)?); }
                _ if ch.is_ascii_digit() => { tokens.push(self.read_number(start)?); }
                _ if is_ident_start(ch) => { tokens.push(self.read_ident(start)); }
                _ => {
                    return Err(format!("unexpected character '{}' at position {}", ch, self.pos));
                }
            }
        }

        Ok(tokens)
    }

    fn read_string(&mut self, start: usize) -> Result<Token, String> {
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
                    None => return Err("unterminated string escape".to_string()),
                },
                Some(c) => value.push(c),
                None => return Err("unterminated string literal".to_string()),
            }
        }
    }

    fn read_number(&mut self, start: usize) -> Result<Token, String> {
        let mut digits = String::new();
        while let Some(ch) = self.peek_char() {
            if ch.is_ascii_digit() {
                digits.push(ch);
                self.next_char();
            } else {
                break;
            }
        }
        let n: usize = digits.parse().map_err(|_| {
            format!("number too large at position {}", start)
        })?;
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

struct Parser {
    input: String,
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(input: &str, tokens: Vec<Token>) -> Self {
        Self { input: input.to_string(), tokens, pos: 0 }
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
            Some(tok) => Err(self.error_at(tok.pos, &format!("expected {:?}, got {:?}", expected, tok.kind))),
            None => Err(self.error_at(self.input.len(), &format!("expected {:?}, got end of input", expected))),
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
        self.tokens.get(self.pos).map(|t| t.pos).unwrap_or(self.input.len())
    }

    fn parse(&mut self) -> Result<Expr, HunksetError> {
        let expr = self.parse_union()?;
        if self.pos < self.tokens.len() {
            let tok = &self.tokens[self.pos];
            return Err(self.error_at(tok.pos, &format!("unexpected {:?}", tok.kind)));
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
    /// error). Use parentheses: `(a ~ b) ~ c`. This matches jj's revset.
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
            let inner = self.parse_negation()?;
            Ok(Expr::Negation(Box::new(inner)))
        } else {
            self.parse_atom()
        }
    }

    fn parse_atom(&mut self) -> Result<Expr, HunksetError> {
        match self.peek() {
            Some(TokenKind::LParen) => {
                self.next_kind();
                let expr = self.parse_union()?;
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
                    Ok(Arg::Pattern(StringPattern {
                        kind: PatternKind::Exact,
                        value: n.to_string(),
                    }))
                }
            }
            Some(TokenKind::Str(_)) => {
                let value = match self.next_kind() {
                    Some(TokenKind::Str(s)) => s,
                    _ => unreachable!(),
                };
                Ok(Arg::Pattern(StringPattern {
                    kind: PatternKind::Substring,
                    value,
                }))
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
                    Ok(Arg::Pattern(StringPattern { kind, value }))
                } else {
                    Ok(Arg::Pattern(StringPattern {
                        kind: PatternKind::Exact,
                        value: ident,
                    }))
                }
            }
            _ => Err(self.error_at(self.current_pos(), "expected argument")),
        }
    }
}

/// Parse a hunkset expression string into an AST.
pub fn parse(input: &str) -> Result<Expr, HunksetError> {
    let mut tokenizer = Tokenizer::new(input);
    let tokens = tokenizer.tokenize().map_err(|e| {
        let pos = e
            .rfind("position ")
            .and_then(|idx| e[idx + 9..].trim().parse::<usize>().ok())
            .unwrap_or(0);
        HunksetError::Parse {
            message: e,
            input: input.to_string(),
            position: pos,
        }
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
        match parse("type(insert) | type(replace) & file(\"x\")").unwrap() {
            Expr::Union(_, right) => assert!(matches!(*right, Expr::Intersection(_, _))),
            other => panic!("expected union, got {:?}", other),
        }
    }

    #[test]
    fn parse_parenthesized() {
        match parse("(type(insert) | type(replace)) & file(\"x\")").unwrap() {
            Expr::Intersection(left, _) => assert!(matches!(*left, Expr::Union(_, _))),
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
                })]
            )
        );
    }

    #[test]
    fn parse_multiple_args() {
        match parse(r#"id("hunk-aabb", "hunk-ccdd")"#).unwrap() {
            Expr::Function(name, args) => {
                assert_eq!(name, "id");
                assert_eq!(args.len(), 2);
            }
            other => panic!("expected function, got {:?}", other),
        }
    }

    #[test]
    fn parse_double_negation() {
        match parse("~~type(delete)").unwrap() {
            Expr::Negation(inner) => assert!(matches!(*inner, Expr::Negation(_))),
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
}
