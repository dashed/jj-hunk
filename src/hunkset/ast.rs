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
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PatternKind {
    Exact,
    Substring,
    Glob,
    Regex,
}
