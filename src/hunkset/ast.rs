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

impl Expr {
    /// Move this node's sub-expressions onto `pending`, leaving leaves behind.
    ///
    /// `Function` holds no `Expr`, so only the operator variants have anything
    /// to hand over.
    fn take_children(&mut self, pending: &mut Vec<Expr>) {
        let mut take = |slot: &mut Box<Expr>| {
            let child = std::mem::replace(&mut **slot, Expr::None);
            // A childless node drops in constant depth; queueing it would just
            // walk the whole tree twice.
            if child.has_children() {
                pending.push(child);
            }
        };
        match self {
            Expr::Union(left, right)
            | Expr::Intersection(left, right)
            | Expr::Difference(left, right) => {
                take(left);
                take(right);
            }
            Expr::Negation(inner) => take(inner),
            Expr::All | Expr::None | Expr::Function(..) => {}
        }
    }

    fn has_children(&self) -> bool {
        matches!(
            self,
            Expr::Union(..) | Expr::Intersection(..) | Expr::Difference(..) | Expr::Negation(..)
        )
    }
}

/// Free an expression tree iteratively.
///
/// The derived drop glue recurses, and `a | b | c | ...` is a left-leaning tree
/// as long as the chain -- so freeing a long chain overflowed the stack even
/// once evaluating it no longer did. Nothing bounds the chain length: it is
/// built in a loop, which is why the parser's nesting limit never sees it.
///
/// Dismantling top-down onto a heap stack keeps teardown at constant depth. The
/// derived `Clone`, `PartialEq` and `Debug` still recurse; none of them is
/// applied to a parsed expression outside tests, and moving those to explicit
/// stacks would cost far more clarity than it buys.
impl Drop for Expr {
    fn drop(&mut self) {
        let mut pending: Vec<Expr> = Vec::new();
        self.take_children(&mut pending);
        while let Some(mut node) = pending.pop() {
            node.take_children(&mut pending);
            // `node` is childless now, so dropping it here bottoms out at once.
        }
    }
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
