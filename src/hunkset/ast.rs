#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    All,
    None,
    Function(String, Vec<Arg>),
    Union(Box<Expr>, Box<Expr>),
    Intersection(Box<Expr>, Box<Expr>),
    Difference(Box<Expr>, Box<Expr>),
    Negation(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Pattern(StringPattern),
    Range(usize, usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct StringPattern {
    pub kind: PatternKind,
    pub value: String,
    /// True when the user wrote an explicit `kind:"value"` prefix.
    ///
    /// Predicates with a natural default (`file()` matches exactly, `glob()`
    /// matches as a glob) may override an *inferred* kind, but must never
    /// override one the user asked for — silently doing so turns a typo'd or
    /// misunderstood query into an empty result.
    pub explicit: bool,
}

impl StringPattern {
    /// A pattern whose kind was inferred, not requested.
    pub fn inferred(kind: PatternKind, value: impl Into<String>) -> Self {
        Self { kind, value: value.into(), explicit: false }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PatternKind {
    Exact,
    Substring,
    Glob,
    Regex,
}
