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
