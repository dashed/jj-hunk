//! Glob matching with the same semantics as `jj`'s `glob:` file patterns.
//!
//! `jj` compiles `glob:"..."` with `globset`'s `GlobBuilder` and
//! `literal_separator(true)`, then matches the compiled pattern against the
//! whole workspace-relative path. This module reproduces that translation
//! (glob -> anchored regex) on top of the `regex` crate, which is already a
//! dependency, so `jj-hunk` and `jj` agree on what a pattern selects.
//!
//! Supported syntax:
//!
//! * `?` - exactly one byte, never `/`.
//! * `*` - zero or more bytes, never `/`. `*` is only a wildcard *within* one
//!   path component: `*.rs` matches `lib.rs` but not `src/lib.rs`.
//! * `**` - zero or more directories, but only when it is a whole path
//!   component. `**/*.rs` matches `.rs` files at any depth (including the top
//!   level), `src/**` matches everything under `src/`. Anywhere else `**` is
//!   just `*`: `a**b` behaves exactly like `a*b`.
//! * `[abc]`, `[a-z]`, `[!a-z]`, `[^a-z]` - character classes. A class is also
//!   how you escape a metacharacter portably: `[*]` is a literal `*`.
//! * `{a,b}` - alternation. Empty branches are dropped, so `x{,y}` is `xy`.
//! * `\` - escapes the next character (on platforms where `\` is not a path
//!   separator).
//!
//! Patterns and paths are normalised the way `jj` normalises repo paths first:
//! `.` components are dropped, repeated separators collapse, and a trailing
//! separator is removed. So `./src//*.rs` is `src/*.rs`, and `src/` is the
//! literal path `src` rather than "everything under `src`" (spell that
//! `src/**`).
//!
//! # Anchoring
//!
//! A pattern always matches the *entire* path, exactly as in `jj`. A pattern
//! with no `/` therefore matches top-level files only. This differs from
//! earlier versions of this fork, where a slash-free pattern was matched
//! against the basename alone and so `*.rs` also matched `src/lib.rs`; write
//! `**/*.rs` for that.
//!
//! Matching is byte-oriented, again like `jj`: `?` consumes one byte, so a
//! two-byte character such as `é` needs `??`.
//!
//! # Errors
//!
//! `jj` rejects malformed patterns (an unclosed `[`, an unbalanced `{`) at
//! parse time. [`glob_match`] has no error channel, so a malformed pattern
//! matches nothing.

use regex::bytes::Regex;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;

/// Whether `\` escapes the next character rather than acting as a separator.
/// Mirrors `globset`'s default, which keys off `std::path::is_separator('\\')`.
const BACKSLASH_ESCAPE: bool = !cfg!(windows);

/// Match a glob pattern against a path.
///
/// See the [module docs](self) for the supported syntax. A malformed pattern
/// matches nothing.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let path = normalize(path);
    let bytes = path.as_bytes();
    with_compiled(pattern, |compiled| match compiled {
        Some(regex) => regex.is_match(bytes),
        None => false,
    })
}

// --- compilation cache ---
//
// Callers glob the same handful of patterns against every file or hunk, so the
// regex is built once per pattern instead of once per candidate.

thread_local! {
    static CACHE: RefCell<HashMap<String, Option<Regex>>> = RefCell::new(HashMap::new());
}

/// Patterns come from user input, so the cache is bounded; it is cleared
/// wholesale rather than evicted, since overflowing it at all is unexpected.
const CACHE_LIMIT: usize = 1024;

fn with_compiled<T>(pattern: &str, f: impl FnOnce(Option<&Regex>) -> T) -> T {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if !cache.contains_key(pattern) {
            if cache.len() >= CACHE_LIMIT {
                cache.clear();
            }
            let compiled = compile(pattern);
            cache.insert(pattern.to_string(), compiled);
        }
        f(cache.get(pattern).and_then(Option::as_ref))
    })
}

fn compile(pattern: &str) -> Option<Regex> {
    let normalized = normalize(pattern);
    let tokens = parse(&normalized).ok()?;
    Regex::new(&tokens_to_anchored_regex(&tokens)).ok()
}

/// Normalise a repo-relative path or pattern: drop `.` components, collapse
/// repeated separators, and strip a trailing separator.
fn normalize(input: &str) -> Cow<'_, str> {
    if !input.split('/').any(|part| part.is_empty() || part == ".") {
        return Cow::Borrowed(input);
    }
    let mut out = String::with_capacity(input.len());
    for part in input.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(part);
    }
    Cow::Owned(out)
}

fn is_separator(c: char) -> bool {
    c == '/' || (cfg!(windows) && c == '\\')
}

// --- parsing ---

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Literal(char),
    /// `?`
    Any,
    /// `*`
    ZeroOrMore,
    /// A leading `**/`.
    RecursivePrefix,
    /// A trailing `/**`.
    RecursiveSuffix,
    /// An interior `/**/`.
    RecursiveZeroOrMore,
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
    Alternates(Vec<Vec<Token>>),
}

/// A malformed pattern. The variants exist to make the parser self-documenting;
/// callers only ever see "this pattern matches nothing".
#[derive(Debug)]
struct ParseError;

struct Parser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    /// Index into `branches` where each open `{` started.
    alternates_stack: Vec<usize>,
    /// Branches of the alternations currently open; tokens are appended to the
    /// last one. `branches[0]` is the top level.
    branches: Vec<Vec<Token>>,
    prev: Option<char>,
    cur: Option<char>,
}

fn parse(pattern: &str) -> Result<Vec<Token>, ParseError> {
    let mut parser = Parser {
        chars: pattern.chars().peekable(),
        alternates_stack: Vec::new(),
        branches: vec![Vec::new()],
        prev: None,
        cur: None,
    };
    parser.parse()?;
    if parser.branches.len() > 1 {
        // An unclosed `{`.
        return Err(ParseError);
    }
    parser.branches.pop().ok_or(ParseError)
}

impl Parser<'_> {
    fn parse(&mut self) -> Result<(), ParseError> {
        while let Some(c) = self.bump() {
            match c {
                '?' => self.push_token(Token::Any)?,
                '*' => self.parse_star()?,
                '[' => self.parse_class()?,
                '{' => self.push_alternate(),
                '}' => self.pop_alternate()?,
                ',' => self.parse_comma()?,
                '\\' => self.parse_backslash()?,
                c => self.push_token(Token::Literal(c))?,
            }
        }
        Ok(())
    }

    fn bump(&mut self) -> Option<char> {
        self.prev = self.cur;
        self.cur = self.chars.next();
        self.cur
    }

    fn peek(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }

    fn push_token(&mut self, token: Token) -> Result<(), ParseError> {
        match self.branches.last_mut() {
            Some(branch) => {
                branch.push(token);
                Ok(())
            }
            None => Err(ParseError),
        }
    }

    fn pop_token(&mut self) -> Result<Token, ParseError> {
        self.branches
            .last_mut()
            .and_then(Vec::pop)
            .ok_or(ParseError)
    }

    fn have_tokens(&self) -> Result<bool, ParseError> {
        match self.branches.last() {
            Some(branch) => Ok(!branch.is_empty()),
            None => Err(ParseError),
        }
    }

    fn push_alternate(&mut self) {
        self.alternates_stack.push(self.branches.len());
        self.branches.push(Vec::new());
    }

    fn pop_alternate(&mut self) -> Result<(), ParseError> {
        // A `}` with no matching `{`.
        let start = self.alternates_stack.pop().ok_or(ParseError)?;
        let alternates = self.branches.drain(start..).collect();
        self.push_token(Token::Alternates(alternates))
    }

    fn parse_comma(&mut self) -> Result<(), ParseError> {
        if self.alternates_stack.is_empty() {
            self.push_token(Token::Literal(','))
        } else {
            self.branches.push(Vec::new());
            Ok(())
        }
    }

    fn parse_backslash(&mut self) -> Result<(), ParseError> {
        if BACKSLASH_ESCAPE {
            match self.bump() {
                // A trailing `\` with nothing to escape.
                None => Err(ParseError),
                Some(c) => self.push_token(Token::Literal(c)),
            }
        } else {
            self.push_token(Token::Literal('/'))
        }
    }

    /// `**` is only recursive when it forms a whole path component; everywhere
    /// else it degrades to two `*`, which is what makes `a**b` mean `a*b`.
    fn parse_star(&mut self) -> Result<(), ParseError> {
        let prev = self.prev;
        if self.peek() != Some('*') {
            return self.push_token(Token::ZeroOrMore);
        }
        self.bump();

        if !self.have_tokens()? {
            if !self.peek().is_none_or(is_separator) {
                // `**foo`: not a component, so plain wildcards.
                self.push_token(Token::ZeroOrMore)?;
                return self.push_token(Token::ZeroOrMore);
            }
            self.push_token(Token::RecursivePrefix)?;
            // Swallow the separator that `RecursivePrefix` accounts for.
            self.bump();
            return Ok(());
        }

        let after_separator = prev.is_some_and(is_separator);
        let in_alternate_head =
            self.branches.len() > 1 && (prev == Some(',') || prev == Some('{'));
        if !after_separator && !in_alternate_head {
            // `foo**`: not a component.
            self.push_token(Token::ZeroOrMore)?;
            return self.push_token(Token::ZeroOrMore);
        }

        let is_suffix = match self.peek() {
            None => true,
            Some(',') | Some('}') if self.branches.len() >= 2 => true,
            Some(c) if is_separator(c) => {
                self.bump();
                false
            }
            _ => {
                // `**foo` after a separator: not a component.
                self.push_token(Token::ZeroOrMore)?;
                return self.push_token(Token::ZeroOrMore);
            }
        };

        // Replace the separator token this `**` absorbs.
        match self.pop_token()? {
            Token::RecursivePrefix => self.push_token(Token::RecursivePrefix),
            Token::RecursiveSuffix => self.push_token(Token::RecursiveSuffix),
            _ if is_suffix => self.push_token(Token::RecursiveSuffix),
            _ => self.push_token(Token::RecursiveZeroOrMore),
        }
    }

    fn parse_class(&mut self) -> Result<(), ParseError> {
        let negated = match self.peek() {
            Some('!') | Some('^') => {
                self.bump();
                true
            }
            _ => false,
        };
        let mut ranges: Vec<(char, char)> = Vec::new();
        let mut first = true;
        let mut in_range = false;
        loop {
            // An unclosed `[`.
            let c = self.bump().ok_or(ParseError)?;
            match c {
                ']' if !first => break,
                // A `]` immediately after `[` (or `[!`) is a literal.
                ']' => ranges.push((']', ']')),
                '-' if first => ranges.push(('-', '-')),
                '-' if in_range => {
                    // A `-` at the end of a range, as in `[a--]`.
                    extend_range(ranges.last_mut().ok_or(ParseError)?, '-')?;
                    in_range = false;
                }
                '-' => in_range = true,
                c if in_range => {
                    extend_range(ranges.last_mut().ok_or(ParseError)?, c)?;
                    in_range = false;
                }
                c => {
                    ranges.push((c, c));
                    in_range = false;
                }
            }
            first = false;
        }
        if in_range {
            // A trailing `-`, as in `[a-]`.
            ranges.push(('-', '-'));
        }
        self.push_token(Token::Class { negated, ranges })
    }
}

fn extend_range(range: &mut (char, char), end: char) -> Result<(), ParseError> {
    range.1 = end;
    if range.1 < range.0 {
        return Err(ParseError);
    }
    Ok(())
}

// --- translation to a regex ---

fn tokens_to_anchored_regex(tokens: &[Token]) -> String {
    // Byte-oriented, like jj: `?` and classes consume single bytes.
    let mut re = String::from("(?-u)^");
    if tokens == [Token::RecursivePrefix] {
        // A bare `**` matches everything, including top-level files.
        re.push_str(".*$");
        return re;
    }
    tokens_to_regex(tokens, &mut re);
    re.push('$');
    re
}

fn tokens_to_regex(tokens: &[Token], re: &mut String) {
    for token in tokens {
        match token {
            Token::Literal(c) => push_escaped_literal(*c, re),
            Token::Any => re.push_str("[^/]"),
            Token::ZeroOrMore => re.push_str("[^/]*"),
            Token::RecursivePrefix => re.push_str("(?:/?|.*/)"),
            Token::RecursiveSuffix => re.push_str("/.*"),
            Token::RecursiveZeroOrMore => re.push_str("(?:/|/.*/)"),
            Token::Class { negated, ranges } => {
                re.push('[');
                if *negated {
                    re.push('^');
                }
                for (start, end) in ranges {
                    push_escaped_literal(*start, re);
                    if start != end {
                        re.push('-');
                        push_escaped_literal(*end, re);
                    }
                }
                re.push(']');
            }
            Token::Alternates(branches) => {
                let parts: Vec<String> = branches
                    .iter()
                    .map(|branch| {
                        let mut part = String::new();
                        tokens_to_regex(branch, &mut part);
                        part
                    })
                    // jj drops empty branches, so `x{,y}` is just `xy`.
                    .filter(|part| !part.is_empty())
                    .collect();
                if !parts.is_empty() {
                    re.push_str("(?:");
                    re.push_str(&parts.join("|"));
                    re.push(')');
                }
            }
        }
    }
}

/// Escape one character for a non-Unicode regex, byte by byte.
fn push_escaped_literal(c: char, re: &mut String) {
    let mut buf = [0u8; 4];
    for &byte in c.encode_utf8(&mut buf).as_bytes() {
        if byte.is_ascii() {
            re.push_str(&regex::escape(char::from(byte).encode_utf8(&mut [0u8; 4])));
        } else {
            re.push_str(&format!("\\x{byte:02x}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The corpus used to check parity with `jj`. Every expectation in this
    /// module was produced by running `jj file list 'glob:"<pattern>"'` in a
    /// scratch repo containing exactly these paths (jj 0.44.0).
    const CORPUS: &[&str] = &[
        ".dotfile",
        ".hidden/h.rs",
        "]",
        "a*b",
        "a.rs",
        "aXb",
        "a_b",
        "ab",
        "br[ack]et.rs",
        "deep/a/b/leaf.rs",
        "lib.rs",
        "main.c",
        "main.h",
        "q}b",
        "sp ace/f o.rs",
        "src/lib.rs",
        "src/main.rs",
        "src/mod.py",
        "src/sub/deep.rs",
        "top.rs",
        "uni/héllo.rs",
        "x{a",
    ];

    /// Assert the full set of corpus paths a pattern selects, exactly as
    /// `jj file list` reported it.
    fn assert_selects(pattern: &str, expected: &[&str]) {
        let got: Vec<&str> = CORPUS
            .iter()
            .copied()
            .filter(|path| glob_match(pattern, path))
            .collect();
        assert_eq!(got, expected, "pattern {pattern:?} selected the wrong set");
    }

    // --- bug 1: character classes ---

    #[test]
    fn character_classes_are_supported() {
        assert_selects("*.[ch]", &["main.c", "main.h"]);
        assert_selects("main.[ch]", &["main.c", "main.h"]);
    }

    #[test]
    fn character_class_ranges() {
        assert_selects("[a-z]*.rs", &["a.rs", "br[ack]et.rs", "lib.rs", "top.rs"]);
        assert!(!glob_match("[A-Z]*.rs", "lib.rs"));
        assert!(glob_match("[A-Z]*.rs", "Lib.rs"));
    }

    #[test]
    fn negated_character_classes() {
        // jj accepts both `!` and `^` as the negation marker.
        assert!(!glob_match("[!l]ib.rs", "lib.rs"));
        assert!(!glob_match("[^l]ib.rs", "lib.rs"));
        assert!(glob_match("[!l]ib.rs", "bib.rs"));
        assert!(glob_match("[^l]ib.rs", "bib.rs"));
    }

    #[test]
    fn character_class_literals() {
        // A leading `]` in a class is a literal `]`.
        assert_selects("[]]", &["]"]);
        // Metacharacters are escaped by putting them in a class.
        assert_selects("[.]dotfile", &[".dotfile"]);
        assert_selects("br[[]ack]et.rs", &["br[ack]et.rs"]);
        assert_selects("q[}]b", &["q}b"]);
        assert_selects("[x][{]a", &["x{a"]);
        assert_selects("a[*]b", &["a*b"]);
    }

    #[test]
    fn unclosed_character_class_matches_nothing() {
        // jj rejects these at parse time; with an infallible API the closest
        // equivalent is a pattern that selects nothing.
        assert_selects("[a", &[]);
        assert_selects("[", &[]);
    }

    // --- bug 2: a slash-free glob is anchored at the repo root ---

    #[test]
    fn slash_free_glob_is_top_level_only() {
        assert_selects("*.rs", &["a.rs", "br[ack]et.rs", "lib.rs", "top.rs"]);
        assert!(!glob_match("*.rs", "src/lib.rs"));
        assert!(!glob_match("*.rs", "deep/a/b/leaf.rs"));
    }

    #[test]
    fn recursive_prefix_matches_any_depth() {
        assert_selects(
            "**/*.rs",
            &[
                ".hidden/h.rs",
                "a.rs",
                "br[ack]et.rs",
                "deep/a/b/leaf.rs",
                "lib.rs",
                "sp ace/f o.rs",
                "src/lib.rs",
                "src/main.rs",
                "src/sub/deep.rs",
                "top.rs",
                "uni/héllo.rs",
            ],
        );
    }

    #[test]
    fn star_does_not_cross_directory_separators() {
        assert_selects(
            "*",
            &[
                ".dotfile",
                "]",
                "a*b",
                "a.rs",
                "aXb",
                "a_b",
                "ab",
                "br[ack]et.rs",
                "lib.rs",
                "main.c",
                "main.h",
                "q}b",
                "top.rs",
                "x{a",
            ],
        );
        assert_selects(
            "*/*.rs",
            &[
                ".hidden/h.rs",
                "sp ace/f o.rs",
                "src/lib.rs",
                "src/main.rs",
                "uni/héllo.rs",
            ],
        );
    }

    #[test]
    fn directory_anchored_globs() {
        assert_selects("src/*.rs", &["src/lib.rs", "src/main.rs"]);
        assert_selects("src/**/*.rs", &["src/lib.rs", "src/main.rs", "src/sub/deep.rs"]);
        assert_selects("src/sub/*.rs", &["src/sub/deep.rs"]);
        assert_selects("deep/**/*.rs", &["deep/a/b/leaf.rs"]);
        assert_selects("deep/*/*/*.rs", &["deep/a/b/leaf.rs"]);
        assert_selects("**/b/*.rs", &["deep/a/b/leaf.rs"]);
        assert_selects("**/leaf.rs", &["deep/a/b/leaf.rs"]);
    }

    // --- bug 3: adding a `*` must never shrink the match set ---

    #[test]
    fn mid_segment_double_star_behaves_like_single_star() {
        // `**` only means "recurse" when it is a whole path component.
        assert_selects("**.rs", &["a.rs", "br[ack]et.rs", "lib.rs", "top.rs"]);
        assert_selects("a**b", &["a*b", "aXb", "a_b", "ab"]);
        assert_selects("a**", &["a*b", "a.rs", "aXb", "a_b", "ab"]);
        assert!(!glob_match("a**b", "a/b"));
    }

    #[test]
    fn adding_a_star_never_shrinks_the_match_set() {
        for path in CORPUS {
            for (narrow, wide) in [("*.rs", "**.rs"), ("a*b", "a**b"), ("*", "**")] {
                if glob_match(narrow, path) {
                    assert!(
                        glob_match(wide, path),
                        "{wide:?} must still match {path:?} that {narrow:?} matched"
                    );
                }
            }
        }
    }

    #[test]
    fn bare_double_star_matches_everything() {
        assert_selects("**", CORPUS);
        assert_selects("**/", CORPUS);
        assert_selects("**/*", CORPUS);
        assert_selects("**/**", CORPUS);
    }

    #[test]
    fn trailing_slash_is_not_a_directory_wildcard() {
        // `src/` is the literal path `src`, not "everything under src".
        assert_selects("src/", &[]);
        assert!(glob_match("src/", "src"));
        assert_selects("lib.rs/", &["lib.rs"]);
        // "everything under src" is spelled `src/**`.
        assert_selects(
            "src/**",
            &["src/lib.rs", "src/main.rs", "src/mod.py", "src/sub/deep.rs"],
        );
        assert_selects(
            "src/**/",
            &["src/lib.rs", "src/main.rs", "src/mod.py", "src/sub/deep.rs"],
        );
        assert_selects("deep/**", &["deep/a/b/leaf.rs"]);
        assert_selects("**/deep/**", &["deep/a/b/leaf.rs"]);
    }

    // --- bug 4: `**` must not blow up ---

    #[test]
    fn many_recursive_wildcards_do_not_blow_up() {
        let path = (0..21).map(|i| format!("d{i}")).collect::<Vec<_>>().join("/");
        let pattern = format!("{}nomatch.rs", "**/".repeat(14));
        let start = std::time::Instant::now();
        assert!(!glob_match(&pattern, &path));
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "matching took {elapsed:?}; the matcher is backtracking exponentially"
        );
    }

    #[test]
    fn many_recursive_wildcards_still_match() {
        let path = (0..21).map(|i| format!("d{i}")).collect::<Vec<_>>().join("/");
        let pattern = format!("{}d20", "**/".repeat(14));
        assert!(glob_match(&pattern, &path));
    }

    // --- bug 5: things that already worked and must keep working ---

    #[test]
    fn question_mark_matches_exactly_one_character() {
        assert_selects("?.rs", &["a.rs"]);
        assert!(!glob_match("?.rs", "ab.rs"));
        assert_selects("l?b.rs", &["lib.rs"]);
        assert_selects("a?b", &["a*b", "aXb", "a_b"]);
        assert!(!glob_match("a?b", "ab"));
        assert!(!glob_match("a?b", "a/b"));
    }

    #[test]
    fn unicode_paths() {
        assert_selects("uni/*.rs", &["uni/héllo.rs"]);
        assert_selects("uni/h*llo.rs", &["uni/héllo.rs"]);
        // jj matches bytes, so `?` consumes one byte: `é` needs two.
        assert_selects("uni/h?llo.rs", &[]);
        assert_selects("uni/h??llo.rs", &["uni/héllo.rs"]);
    }

    #[test]
    fn spaces_in_paths() {
        assert_selects("sp ace/*.rs", &["sp ace/f o.rs"]);
        assert_selects("sp ace/f o.rs", &["sp ace/f o.rs"]);
        assert_selects("*/f*.rs", &["sp ace/f o.rs"]);
    }

    #[test]
    fn leading_dot_slash_is_stripped() {
        assert_selects("./*.rs", &["a.rs", "br[ack]et.rs", "lib.rs", "top.rs"]);
        assert!(glob_match("*.rs", "./lib.rs"));
        assert!(glob_match("./src/*.rs", "src/lib.rs"));
        assert!(glob_match("src/*.rs", "./src/lib.rs"));
    }

    #[test]
    fn repeated_separators_collapse() {
        assert_selects("src//*.rs", &["src/lib.rs", "src/main.rs"]);
        assert!(glob_match("src/*.rs", "src//lib.rs"));
    }

    #[test]
    fn exact_path() {
        assert_selects("src/lib.rs", &["src/lib.rs"]);
        assert!(!glob_match("src/lib.rs", "src/main.rs"));
    }

    #[test]
    fn non_matching_extension() {
        assert!(!glob_match("*.rs", "lib.py"));
    }

    // --- brace alternation (jj supports it; the old matcher did not) ---

    #[test]
    fn brace_alternation() {
        assert_selects("*.{c,h}", &["main.c", "main.h"]);
        assert_selects("{src,uni}/*.rs", &["src/lib.rs", "src/main.rs", "uni/héllo.rs"]);
        assert_selects("**/{a,lib}.rs", &["a.rs", "lib.rs", "src/lib.rs"]);
        assert_selects(
            "**/*.{rs,py}",
            &[
                ".hidden/h.rs",
                "a.rs",
                "br[ack]et.rs",
                "deep/a/b/leaf.rs",
                "lib.rs",
                "sp ace/f o.rs",
                "src/lib.rs",
                "src/main.rs",
                "src/mod.py",
                "src/sub/deep.rs",
                "top.rs",
                "uni/héllo.rs",
            ],
        );
    }

    #[test]
    fn empty_brace_alternates_are_dropped() {
        // jj drops empty alternates, so this does *not* also match `src/*.rs`.
        assert_selects("src{,/sub}/*.rs", &["src/sub/deep.rs"]);
    }

    #[test]
    fn unbalanced_braces_match_nothing() {
        assert_selects("x{a", &[]);
        assert_selects("}b", &[]);
    }

    // --- backslash escapes ---

    #[test]
    fn backslash_escapes_metacharacters() {
        assert_selects(r"a\*b", &["a*b"]);
        assert!(!glob_match(r"a\*b", "aXb"));
        assert_selects(r"a\?b", &[]);
    }

    // --- degenerate patterns ---

    #[test]
    fn degenerate_patterns_match_nothing() {
        assert_selects("", &[]);
        assert_selects(".", &[]);
        assert_selects("./", &[]);
        assert_selects("a/**/b", &[]);
    }

    // --- non-path haystacks (hunkset matches globs against these too) ---

    #[test]
    fn matches_plain_strings_without_separators() {
        assert!(glob_match("rs", "rs"));
        assert!(glob_match("r*", "rs"));
        assert!(glob_match("*", "modified"));
        assert!(glob_match("handle_*", "handle_click"));
        assert!(!glob_match("handle_*", "on_click"));
    }
}
