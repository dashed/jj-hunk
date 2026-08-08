use thiserror::Error;

#[derive(Debug, Error)]
pub enum HunksetError {
    #[error("{message}")]
    Parse {
        message: String,
        input: String,
        position: usize,
    },
    #[error("unknown function '{name}'")]
    UnknownFunction { name: String },
    #[error("invalid regex '{pattern}': {source}")]
    InvalidRegex {
        pattern: String,
        source: regex::Error,
    },
    #[error("hunk id '{prefix}' is ambiguous -- it matches {count} hunks: {candidates}. Use more characters, or exact:\"<full-id>\".")]
    AmbiguousId {
        prefix: String,
        count: usize,
        candidates: String,
    },
    #[error("{func}() does not accept '{value}' -- valid values are: {valid}")]
    InvalidArgument {
        func: String,
        value: String,
        valid: String,
    },
    #[error("{name}() requires the 'semantic' feature (build with --features semantic)")]
    #[allow(dead_code)] // only constructed when semantic feature is disabled
    SemanticFeatureRequired { name: String },
}

impl HunksetError {
    /// Format the error with a caret pointing at the position in the input.
    pub fn display_with_context(&self) -> String {
        match self {
            HunksetError::Parse { message, input, position } => {
                let caret = format!("{}^", " ".repeat(*position));
                format!("{}\n{}\n{}", input, caret, message)
            }
            other => format!("{}", other),
        }
    }
}
