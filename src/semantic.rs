use tree_sitter::{Language, Parser, Tree};

/// Semantic context extracted for a given line in a source file.
#[derive(Debug, Clone, Default)]
pub struct SemanticContext {
    /// Name of the enclosing function/method, if any.
    pub enclosing_function: Option<String>,
    /// Name of the enclosing scope (class, struct, impl, module, etc.), if any.
    pub enclosing_scope: Option<String>,
    /// Annotations/decorators on the enclosing function/scope (e.g., `#[test]`, `@Override`).
    pub annotations: Vec<String>,
    /// Whether the line is inside a doc comment or docstring.
    pub is_doc_comment: bool,
    /// Whether the line is an import/use/require statement.
    pub is_import: bool,
    /// Whether the line is at the top level (not inside any function or scope).
    pub is_toplevel: bool,
    /// Nesting depth of enclosing scopes (0 = top level).
    pub nesting_depth: usize,
    /// Whether a parser actually ran for this file.
    ///
    /// Without this, an unsupported file type is indistinguishable from a
    /// genuinely top-level line: `SemanticContext::default()` is depth 0 but
    /// `is_toplevel` false, so `depth(0)` matched it while `toplevel()` did
    /// not. Predicates consult this so both fail the same way.
    pub is_analyzed: bool,
}

/// Language configuration: which node kinds represent functions and scopes.
struct LangConfig {
    language: Language,
    function_kinds: &'static [&'static str],
    scope_kinds: &'static [&'static str],
    /// How to extract the name from a function node.
    function_name_extractor: NameExtractor,
    /// How to extract the name from a scope node (defaults to NameField).
    scope_name_extractor: NameExtractor,
    /// Node kinds that represent annotations/decorators.
    annotation_kinds: &'static [&'static str],
    /// Node kinds that represent comments (candidates for doc comments).
    comment_kinds: &'static [&'static str],
    /// How to determine if a comment is a doc comment.
    doc_comment_check: DocCommentCheck,
    /// Node kinds that represent import/use/require statements.
    import_kinds: &'static [&'static str],
    /// For macro-based languages (e.g., Elixir): `call` node target identifiers
    /// that define functions (e.g., "def", "defp").
    call_function_targets: &'static [&'static str],
    /// For macro-based languages: `call` node target identifiers that define
    /// scopes (e.g., "defmodule").
    call_scope_targets: &'static [&'static str],
    /// For macro-based languages: `call` node target identifiers that represent
    /// imports (e.g., "import", "use", "require").
    call_import_targets: &'static [&'static str],
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
enum NameExtractor {
    /// Use the "name" field directly.
    NameField,
    /// Also try the "type" field (Rust impl_item).
    NameOrTypeField,
    /// C/C++ style: dig into declarator → function_declarator → declarator.
    CDeclarator,
    /// Find first child of a given kind (e.g., Kotlin simple_identifier).
    FirstChildOfKind(&'static str),
    /// OCaml let_binding: use "pattern" field.
    PatternField,
    /// Try "name" field, then fall back to first "field_identifier" child (Go methods).
    NameOrFieldIdentifier,
}

#[derive(Clone, Copy)]
enum DocCommentCheck {
    /// Check text starts with any of these prefixes (e.g., "///", "//!")
    Prefixes(&'static [&'static str]),
    /// Any comment on this node kind is a doc comment (e.g., dedicated doc_comment node)
    NodeKind(&'static str),
    /// A comment is a doc comment if its next named sibling is a declaration.
    /// The list contains declaration node kinds to check for.
    BeforeDeclaration(&'static [&'static str]),
    /// Python-style: a string literal that is the first statement in a
    /// function/class/module body (docstring).
    PythonDocstring,
    /// No doc comment detection for this language
    None,
}

fn get_lang_config(extension: &str) -> Option<LangConfig> {
    match extension {
        "rs" => Some(LangConfig {
            language: tree_sitter_rust::LANGUAGE.into(),
            function_kinds: &["function_item"],
            scope_kinds: &["impl_item", "struct_item", "enum_item", "mod_item", "trait_item"],
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::NameOrTypeField,
            annotation_kinds: &["attribute_item", "inner_attribute_item"],
            comment_kinds: &["line_comment", "block_comment"],
            doc_comment_check: DocCommentCheck::Prefixes(&["///", "//!", "/**"]),
            import_kinds: &["use_declaration", "extern_crate_declaration"],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "py" => Some(LangConfig {
            language: tree_sitter_python::LANGUAGE.into(),
            function_kinds: &["function_definition"],
            // NOTE: do not add "module" here. In tree-sitter-python `module`
            // is the ROOT node, so listing it as a scope makes every line in
            // the file non-top-level and depth >= 1. Ruby's "module" below is
            // a real language construct (its root is `program`), so that one
            // is correct.
            scope_kinds: &["class_definition"],
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &["decorator"],
            comment_kinds: &["comment", "expression_statement"],
            doc_comment_check: DocCommentCheck::PythonDocstring,
            import_kinds: &["import_statement", "import_from_statement"],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "js" | "mjs" | "cjs" | "jsx" => Some(LangConfig {
            language: tree_sitter_javascript::LANGUAGE.into(),
            function_kinds: &["function_declaration", "method_definition", "arrow_function"],
            scope_kinds: &["class_declaration"],
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &["decorator"],
            comment_kinds: &["comment"],
            doc_comment_check: DocCommentCheck::Prefixes(&["/**"]),
            import_kinds: &["import_statement"],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "ts" | "tsx" => {
            let language = if extension == "tsx" {
                tree_sitter_typescript::LANGUAGE_TSX.into()
            } else {
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
            };
            Some(LangConfig {
                language,
                function_kinds: &["function_declaration", "method_definition", "arrow_function"],
                scope_kinds: &["class_declaration", "interface_declaration"],
                function_name_extractor: NameExtractor::NameField,
                scope_name_extractor: NameExtractor::NameField,
                annotation_kinds: &["decorator"],
                comment_kinds: &["comment"],
                doc_comment_check: DocCommentCheck::Prefixes(&["/**"]),
                import_kinds: &["import_statement"],
                call_function_targets: &[],
                call_scope_targets: &[],
                call_import_targets: &[],
            })
        }
        "go" => Some(LangConfig {
            language: tree_sitter_go::LANGUAGE.into(),
            function_kinds: &["function_declaration", "method_declaration"],
            scope_kinds: &["type_declaration"],
            function_name_extractor: NameExtractor::NameOrFieldIdentifier,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &[],
            comment_kinds: &["comment"],
            doc_comment_check: DocCommentCheck::BeforeDeclaration(&[
                "function_declaration", "method_declaration", "type_declaration",
                "var_declaration", "const_declaration",
            ]),
            import_kinds: &["import_declaration"],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "c" | "h" => Some(LangConfig {
            language: tree_sitter_c::LANGUAGE.into(),
            function_kinds: &["function_definition"],
            scope_kinds: &["struct_specifier", "enum_specifier"],
            function_name_extractor: NameExtractor::CDeclarator,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &[],
            comment_kinds: &["comment"],
            doc_comment_check: DocCommentCheck::Prefixes(&["/**", "///"]),
            import_kinds: &["preproc_include"],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "cc" | "cpp" | "cxx" | "hpp" | "hxx" | "hh" => Some(LangConfig {
            language: tree_sitter_cpp::LANGUAGE.into(),
            function_kinds: &["function_definition"],
            scope_kinds: &["class_specifier", "struct_specifier", "namespace_definition"],
            function_name_extractor: NameExtractor::CDeclarator,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &[],
            comment_kinds: &["comment"],
            doc_comment_check: DocCommentCheck::Prefixes(&["/**", "///", "//!"]),
            import_kinds: &["preproc_include", "using_declaration"],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "java" => Some(LangConfig {
            language: tree_sitter_java::LANGUAGE.into(),
            function_kinds: &["method_declaration", "constructor_declaration"],
            scope_kinds: &["class_declaration", "interface_declaration", "enum_declaration"],
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &["marker_annotation", "annotation"],
            comment_kinds: &["line_comment", "block_comment"],
            doc_comment_check: DocCommentCheck::Prefixes(&["/**"]),
            import_kinds: &["import_declaration"],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "rb" => Some(LangConfig {
            language: tree_sitter_ruby::LANGUAGE.into(),
            function_kinds: &["method", "singleton_method"],
            scope_kinds: &["class", "module"],
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &[],
            comment_kinds: &["comment"],
            doc_comment_check: DocCommentCheck::None,
            import_kinds: &[],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &["require", "require_relative", "load"],
        }),
        "cs" => Some(LangConfig {
            language: tree_sitter_c_sharp::LANGUAGE.into(),
            function_kinds: &["method_declaration", "constructor_declaration"],
            scope_kinds: &["class_declaration", "struct_declaration", "namespace_declaration", "interface_declaration"],
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &["attribute_list"],
            comment_kinds: &["comment"],
            doc_comment_check: DocCommentCheck::Prefixes(&["///", "/**"]),
            import_kinds: &["using_directive"],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        // --- Additional languages ---
        // Kotlin: tree-sitter-kotlin crate uses old tree-sitter API, not yet
        // compatible with tree-sitter 0.25+. Re-add when the crate is updated.
        "scala" | "sc" => Some(LangConfig {
            language: tree_sitter_scala::LANGUAGE.into(),
            function_kinds: &["function_definition", "function_declaration"],
            scope_kinds: &["class_definition", "object_definition", "trait_definition", "enum_definition"],
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &["annotation"],
            comment_kinds: &["comment", "block_comment"],
            doc_comment_check: DocCommentCheck::Prefixes(&["/**"]),
            import_kinds: &["import_declaration"],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "swift" => Some(LangConfig {
            language: tree_sitter_swift::LANGUAGE.into(),
            function_kinds: &["function_declaration", "init_declaration"],
            scope_kinds: &["class_declaration"],
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &["attribute"],
            comment_kinds: &["comment", "multiline_comment"],
            doc_comment_check: DocCommentCheck::Prefixes(&["///", "/**"]),
            import_kinds: &["import_declaration"],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "php" => Some(LangConfig {
            language: tree_sitter_php::LANGUAGE_PHP.into(),
            function_kinds: &["function_definition", "method_declaration"],
            scope_kinds: &["class_declaration", "interface_declaration", "trait_declaration", "enum_declaration", "namespace_definition"],
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &["attribute_list"],
            comment_kinds: &["comment"],
            doc_comment_check: DocCommentCheck::Prefixes(&["/**"]),
            import_kinds: &["namespace_use_declaration"],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "sh" | "bash" => Some(LangConfig {
            language: tree_sitter_bash::LANGUAGE.into(),
            function_kinds: &["function_definition"],
            scope_kinds: &[],
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &[],
            comment_kinds: &["comment"],
            doc_comment_check: DocCommentCheck::None,
            import_kinds: &[],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "ex" | "exs" => Some(LangConfig {
            language: tree_sitter_elixir::LANGUAGE.into(),
            function_kinds: &[],
            scope_kinds: &[],
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &[],
            comment_kinds: &["comment"],
            doc_comment_check: DocCommentCheck::None,
            import_kinds: &[],
            // Elixir uses macros: def/defp/defmodule/import/use/require are `call` nodes
            call_function_targets: &["def", "defp"],
            call_scope_targets: &["defmodule", "defprotocol", "defimpl"],
            call_import_targets: &["import", "use", "require", "alias"],
        }),
        "erl" | "hrl" => Some(LangConfig {
            language: tree_sitter_erlang::LANGUAGE.into(),
            function_kinds: &["function_clause"],
            scope_kinds: &[],
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &[],
            comment_kinds: &["comment"],
            doc_comment_check: DocCommentCheck::None,
            import_kinds: &[],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "hs" | "lhs" => Some(LangConfig {
            language: tree_sitter_haskell::LANGUAGE.into(),
            function_kinds: &["function"],
            scope_kinds: &["class", "data_type", "newtype"],
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &[],
            comment_kinds: &["comment"],
            doc_comment_check: DocCommentCheck::Prefixes(&["-- |", "-- ^"]),
            import_kinds: &["import"],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "ml" | "mli" => Some(LangConfig {
            language: tree_sitter_ocaml::LANGUAGE_OCAML.into(),
            function_kinds: &["let_binding"], // matches all let bindings, not just function-valued
            scope_kinds: &["module_binding"],
            function_name_extractor: NameExtractor::PatternField,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &[],
            comment_kinds: &["comment"],
            doc_comment_check: DocCommentCheck::Prefixes(&["(**"]),
            import_kinds: &["open_statement"],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "zig" => Some(LangConfig {
            language: tree_sitter_zig::LANGUAGE.into(),
            function_kinds: &["function_declaration"],
            scope_kinds: &[],
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &[],
            comment_kinds: &["line_comment", "doc_comment"],
            doc_comment_check: DocCommentCheck::NodeKind("doc_comment"),
            import_kinds: &[], // @import is a builtin_call_expression but so are @embedFile, @cImport, etc.
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "lua" => Some(LangConfig {
            language: tree_sitter_lua::LANGUAGE.into(),
            function_kinds: &["function_declaration"],
            scope_kinds: &[],
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &[],
            comment_kinds: &["comment"],
            doc_comment_check: DocCommentCheck::Prefixes(&["---"]),
            import_kinds: &[],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        _ => None,
    }
}

/// A parsed source file that can answer semantic queries about line ranges.
pub struct ParsedFile {
    tree: Tree,
    source: String,
    config: LangConfig,
}

impl ParsedFile {
    /// Parse a source file given its extension (e.g., "rs", "py") and content.
    /// Returns None if the language is not supported.
    pub fn parse(extension: &str, source: &str) -> Option<Self> {
        let config = get_lang_config(extension)?;
        let mut parser = Parser::new();
        parser.set_language(&config.language).ok()?;
        let tree = parser.parse(source.as_bytes(), None)?;
        Some(Self {
            tree,
            source: source.to_string(),
            config,
        })
    }

    /// Get the semantic context (enclosing function/scope) for a given
    /// 1-based line number.
    #[allow(dead_code)]
    pub fn context_at_line(&self, line: usize) -> SemanticContext {
        self.contexts_at_lines(&[line]).into_iter().next().unwrap_or_default()
    }

    /// Get semantic contexts for multiple 1-based line numbers in a single
    /// tree walk per line. The tree is parsed once and reused.
    pub fn contexts_at_lines(&self, lines: &[usize]) -> Vec<SemanticContext> {
        let root = self.tree.root_node();
        lines
            .iter()
            .map(|&line| {
                if line == 0 {
                    return SemanticContext::default();
                }
                let line_0 = line - 1;
                let mut ctx = SemanticContext::default();
                ctx.is_toplevel = true;
                ctx.is_analyzed = true;
                self.find_enclosing(root, line_0, &mut ctx, 0);
                ctx
            })
            .collect()
    }

    /// Recursively walk the tree to find the innermost function and scope
    /// containing the given 0-based line.
    ///
    /// Invariant: since we recurse depth-first and inner nodes are visited
    /// after outer ones, `enclosing_function` and `enclosing_scope` are
    /// overwritten as we descend — the last match is the innermost.
    fn find_enclosing(
        &self,
        node: tree_sitter::Node,
        line: usize,
        ctx: &mut SemanticContext,
        depth: usize,
    ) {
        let kind = node.kind();

        // Check if this node IS a doc-comment/import at the target line
        if self.config.comment_kinds.contains(&kind) && node_contains_line(&node, line) {
            ctx.is_doc_comment = self.is_doc_comment(&node);
        }

        if self.config.import_kinds.contains(&kind) && node_contains_line(&node, line) {
            ctx.is_import = true;
        }

        // Check function/scope containment
        let mut entered_scope = false;

        if self.config.function_kinds.contains(&kind) {
            if node_contains_line(&node, line) {
                if let Some(name) = self.extract_function_name(&node) {
                    ctx.enclosing_function = Some(name);
                }
                ctx.is_toplevel = false;
                entered_scope = true;
                self.collect_sibling_annotations(&node, ctx);
            }
        }

        if self.config.scope_kinds.contains(&kind) {
            if node_contains_line(&node, line) {
                if let Some(name) = self.extract_node_name(&node, self.config.scope_name_extractor) {
                    ctx.enclosing_scope = Some(name);
                }
                ctx.is_toplevel = false;
                entered_scope = true;
                self.collect_sibling_annotations(&node, ctx);
            }
        }

        // Call-based detection for macro-heavy languages (e.g., Elixir).
        // Check if this is a `call` node whose target matches a known keyword.
        if kind == "call" && node_contains_line(&node, line) {
            if let Some(target) = self.call_target_text(&node) {
                if self.config.call_function_targets.contains(&target.as_str()) {
                    if let Some(name) = self.extract_call_arg_name(&node) {
                        ctx.enclosing_function = Some(name);
                    }
                    ctx.is_toplevel = false;
                    entered_scope = true;
                } else if self.config.call_scope_targets.contains(&target.as_str()) {
                    if let Some(name) = self.extract_call_arg_name(&node) {
                        ctx.enclosing_scope = Some(name);
                    }
                    ctx.is_toplevel = false;
                    entered_scope = true;
                } else if self.config.call_import_targets.contains(&target.as_str()) {
                    ctx.is_import = true;
                }
            }
        }

        let new_depth = if entered_scope { depth + 1 } else { depth };
        // Track the deepest nesting level reached
        if new_depth > ctx.nesting_depth {
            ctx.nesting_depth = new_depth;
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if node_contains_line(&child, line) {
                self.find_enclosing(child, line, ctx, new_depth);
            }
        }
    }

    /// Collect annotation nodes attached to a function/scope node.
    /// Checks both preceding siblings and child nodes (different grammars
    /// place annotations differently).
    fn collect_sibling_annotations(&self, node: &tree_sitter::Node, ctx: &mut SemanticContext) {
        // Check preceding siblings (Rust, Python decorators)
        let mut sibling = node.prev_named_sibling();
        while let Some(sib) = sibling {
            if self.config.annotation_kinds.contains(&sib.kind()) {
                self.add_annotation_text(&sib, ctx);
            } else {
                break;
            }
            sibling = sib.prev_named_sibling();
        }

        // Check child nodes (Java, C# — annotations inside modifiers or directly)
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if self.config.annotation_kinds.contains(&child.kind()) {
                self.add_annotation_text(&child, ctx);
            }
            // Also check inside "modifiers" wrapper nodes (Java)
            if child.kind() == "modifiers" {
                let mut inner_cursor = child.walk();
                for grandchild in child.children(&mut inner_cursor) {
                    if self.config.annotation_kinds.contains(&grandchild.kind()) {
                        self.add_annotation_text(&grandchild, ctx);
                    }
                }
            }
        }
    }

    fn is_doc_comment(&self, node: &tree_sitter::Node) -> bool {
        match self.config.doc_comment_check {
            DocCommentCheck::Prefixes(prefixes) => {
                if let Ok(text) = node.utf8_text(self.source.as_bytes()) {
                    let trimmed = text.trim_start();
                    prefixes.iter().any(|prefix| trimmed.starts_with(prefix))
                } else {
                    false
                }
            }
            DocCommentCheck::NodeKind(kind) => node.kind() == kind,
            DocCommentCheck::BeforeDeclaration(decl_kinds) => {
                // A comment is a doc comment if its next named sibling is a
                // declaration. We skip over other comment nodes to handle
                // multi-line doc comment blocks.
                let mut sibling = node.next_named_sibling();
                while let Some(sib) = sibling {
                    if self.config.comment_kinds.contains(&sib.kind()) {
                        sibling = sib.next_named_sibling();
                        continue;
                    }
                    return decl_kinds.contains(&sib.kind());
                }
                false
            }
            DocCommentCheck::PythonDocstring => {
                // A docstring is an expression_statement containing a string
                // that is the first statement in a block (function/class body).
                if node.kind() == "comment" {
                    return false; // Regular # comments are never docstrings
                }
                if node.kind() != "expression_statement" {
                    return false;
                }
                // Must contain a string child
                let has_string = node.named_child(0)
                    .map(|c| c.kind() == "string")
                    .unwrap_or(false);
                if !has_string {
                    return false;
                }
                // Must be the first named child of its parent (the block)
                if let Some(parent) = node.parent() {
                    parent.named_child(0).map(|first| first.id() == node.id()).unwrap_or(false)
                } else {
                    false
                }
            }
            DocCommentCheck::None => false,
        }
    }

    /// Get the target identifier text of a `call` node (e.g., "def", "defmodule").
    /// Get the target/method identifier text of a `call` node.
    /// Tries `target` field (Elixir) then `method` field (Ruby).
    fn call_target_text(&self, node: &tree_sitter::Node) -> Option<String> {
        let target = node.child_by_field_name("target")
            .or_else(|| node.child_by_field_name("method"))?;
        target.utf8_text(self.source.as_bytes()).ok().map(|s| s.to_string())
    }

    /// Extract the name from the first argument of a `call` node.
    /// Handles both `def foo(args)` (first arg is a call whose target is the name)
    /// and `def foo` (first arg is an identifier) and `defmodule Foo` (first arg
    /// is an alias).
    fn extract_call_arg_name(&self, node: &tree_sitter::Node) -> Option<String> {
        // Find the `arguments` child by kind (not field name — Elixir's grammar
        // doesn't use field names for arguments).
        let args = find_child_by_kind(node, "arguments")?;
        let first_arg = args.named_child(0)?;
        match first_arg.kind() {
            // def get_user(id) — first arg is a call node, name is its target
            "call" => {
                let target = first_arg.child_by_field_name("target")?;
                target.utf8_text(self.source.as_bytes()).ok().map(|s| s.to_string())
            }
            // def foo / defmodule Foo — first arg is an identifier or alias
            _ => first_arg.utf8_text(self.source.as_bytes()).ok().map(|s| s.to_string()),
        }
    }

    fn add_annotation_text(&self, node: &tree_sitter::Node, ctx: &mut SemanticContext) {
        if let Ok(text) = node.utf8_text(self.source.as_bytes()) {
            let text = text.trim().to_string();
            if !ctx.annotations.contains(&text) {
                ctx.annotations.push(text);
            }
        }
    }

    fn extract_function_name(&self, node: &tree_sitter::Node) -> Option<String> {
        self.extract_node_name(node, self.config.function_name_extractor)
    }

    fn extract_node_name(&self, node: &tree_sitter::Node, extractor: NameExtractor) -> Option<String> {
        match extractor {
            NameExtractor::NameField => {
                let name_node = node.child_by_field_name("name")?;
                name_node.utf8_text(self.source.as_bytes()).ok().map(|s| s.to_string())
            }
            NameExtractor::NameOrTypeField => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    return name_node.utf8_text(self.source.as_bytes()).ok().map(|s| s.to_string());
                }
                let type_node = node.child_by_field_name("type")?;
                type_node.utf8_text(self.source.as_bytes()).ok().map(|s| s.to_string())
            }
            NameExtractor::CDeclarator => self.extract_c_function_name(node),
            NameExtractor::FirstChildOfKind(kind) => {
                find_child_by_kind(node, kind)
                    .and_then(|n| n.utf8_text(self.source.as_bytes()).ok().map(|s| s.to_string()))
            }
            NameExtractor::PatternField => {
                let pat_node = node.child_by_field_name("pattern")?;
                pat_node.utf8_text(self.source.as_bytes()).ok().map(|s| s.to_string())
            }
            NameExtractor::NameOrFieldIdentifier => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    return name_node.utf8_text(self.source.as_bytes()).ok().map(|s| s.to_string());
                }
                find_child_by_kind(node, "field_identifier")
                    .and_then(|n| n.utf8_text(self.source.as_bytes()).ok().map(|s| s.to_string()))
            }
        }
    }

    /// For C/C++: function name is inside declarator → (function_declarator →) declarator
    fn extract_c_function_name(&self, node: &tree_sitter::Node) -> Option<String> {
        let declarator = node.child_by_field_name("declarator")?;
        // Could be function_declarator or pointer_declarator wrapping it
        let func_decl = if declarator.kind() == "function_declarator" {
            declarator
        } else {
            // Try children
            find_child_by_kind(&declarator, "function_declarator")?
        };
        let name_node = func_decl.child_by_field_name("declarator")?;
        // This might be an identifier or a qualified_identifier
        let name = self.leaf_text(&name_node)?;
        Some(name)
    }

    fn leaf_text(&self, node: &tree_sitter::Node) -> Option<String> {
        if node.child_count() == 0 {
            return node.utf8_text(self.source.as_bytes()).ok().map(|s| s.to_string());
        }
        // For qualified identifiers, get the full text
        node.utf8_text(self.source.as_bytes()).ok().map(|s| s.to_string())
    }
}

fn node_contains_line(node: &tree_sitter::Node, line: usize) -> bool {
    node.start_position().row <= line && node.end_position().row >= line
}

fn find_child_by_kind<'a>(node: &tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == kind {
            return Some(child);
        }
    }
    None
}

/// Get semantic contexts for multiple lines in a file, reusing a single parse.
/// Returns a Vec of SemanticContext, one per input line number.
#[allow(dead_code)]
pub fn contexts_for_lines(extension: &str, source: &str, lines: &[usize]) -> Vec<SemanticContext> {
    match ParsedFile::parse(extension, source) {
        Some(parsed) => parsed.contexts_at_lines(lines),
        None => lines.iter().map(|_| SemanticContext::default()).collect(),
    }
}

/// Extract the file extension from a path.
#[allow(dead_code)]
pub fn extension_from_path(path: &str) -> &str {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_function_detection() {
        let source = r#"
fn top_level() {
    let x = 1;
}

struct Foo;

impl Foo {
    fn method(&self) {
        let y = 2;
    }
}
"#;
        let parsed = ParsedFile::parse("rs", source).unwrap();

        // Line 3: inside top_level
        let ctx = parsed.context_at_line(3);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("top_level"));
        assert_eq!(ctx.enclosing_scope, None);

        // Line 10: inside Foo::method
        let ctx = parsed.context_at_line(10);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("method"));
        assert_eq!(ctx.enclosing_scope.as_deref(), Some("Foo"));
    }

    #[test]
    fn python_function_detection() {
        let source = r#"
class MyClass:
    def my_method(self):
        x = 1
        return x

def standalone():
    pass
"#;
        let parsed = ParsedFile::parse("py", source).unwrap();

        // Line 4: inside MyClass.my_method
        let ctx = parsed.context_at_line(4);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("my_method"));
        assert_eq!(ctx.enclosing_scope.as_deref(), Some("MyClass"));

        // Line 8: inside standalone
        let ctx = parsed.context_at_line(8);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("standalone"));
    }

    #[test]
    fn javascript_function_detection() {
        let source = r#"
function hello() {
    console.log("hi");
}

class Widget {
    render() {
        return null;
    }
}
"#;
        let parsed = ParsedFile::parse("js", source).unwrap();

        let ctx = parsed.context_at_line(3);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("hello"));

        let ctx = parsed.context_at_line(8);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("render"));
        assert_eq!(ctx.enclosing_scope.as_deref(), Some("Widget"));
    }

    #[test]
    fn go_function_detection() {
        let source = r#"package main

func hello() {
    fmt.Println("hi")
}
"#;
        let parsed = ParsedFile::parse("go", source).unwrap();

        let ctx = parsed.context_at_line(4);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("hello"));
    }

    #[test]
    fn c_function_detection() {
        let source = r#"
int main(int argc, char **argv) {
    return 0;
}
"#;
        let parsed = ParsedFile::parse("c", source).unwrap();

        let ctx = parsed.context_at_line(3);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("main"));
    }

    #[test]
    fn unsupported_extension_returns_none() {
        assert!(ParsedFile::parse("xyz", "anything").is_none());
    }

    #[test]
    fn batch_contexts() {
        let source = r#"
fn alpha() {
    let a = 1;
}

fn beta() {
    let b = 2;
}
"#;
        let results = contexts_for_lines("rs", source, &[3, 7]);
        assert_eq!(results[0].enclosing_function.as_deref(), Some("alpha"));
        assert_eq!(results[1].enclosing_function.as_deref(), Some("beta"));
    }

    #[test]
    fn scala_function_detection() {
        let source = r#"
object Main {
  def hello(): Unit = {
    println("hi")
  }
}
"#;
        let parsed = ParsedFile::parse("scala", source).unwrap();
        let ctx = parsed.context_at_line(4);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("hello"));
        assert_eq!(ctx.enclosing_scope.as_deref(), Some("Main"));
    }

    #[test]
    fn php_function_detection() {
        let source = r#"<?php
class UserService {
    public function getUser($id) {
        return $id;
    }
}
?>"#;
        let parsed = ParsedFile::parse("php", source).unwrap();
        let ctx = parsed.context_at_line(4);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("getUser"));
        assert_eq!(ctx.enclosing_scope.as_deref(), Some("UserService"));
    }

    #[test]
    fn bash_function_detection() {
        let source = r#"#!/bin/bash
my_func() {
    echo "hello"
}
"#;
        let parsed = ParsedFile::parse("sh", source).unwrap();
        let ctx = parsed.context_at_line(3);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("my_func"));
    }

    #[test]
    fn haskell_function_detection() {
        let source = r#"
module Main where

factorial :: Int -> Int
factorial 0 = 1
factorial n = n * factorial (n - 1)
"#;
        let parsed = ParsedFile::parse("hs", source).unwrap();
        let ctx = parsed.context_at_line(6);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("factorial"));
    }

    #[test]
    fn zig_function_detection() {
        let source = r#"
const std = @import("std");

fn add(a: i32, b: i32) i32 {
    return a + b;
}
"#;
        let parsed = ParsedFile::parse("zig", source).unwrap();
        let ctx = parsed.context_at_line(5);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("add"));
    }

    #[test]
    fn lua_function_detection() {
        let source = r#"
function greet(name)
    print("Hello, " .. name)
end
"#;
        let parsed = ParsedFile::parse("lua", source).unwrap();
        let ctx = parsed.context_at_line(3);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("greet"));
    }

    #[test]
    fn erlang_function_detection() {
        let source = r#"-module(hello).
-export([greet/0]).

greet() ->
    io:format("Hello~n").
"#;
        let parsed = ParsedFile::parse("erl", source).unwrap();
        let ctx = parsed.context_at_line(5);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("greet"));
    }

    // --- tests for new semantic predicates ---

    #[test]
    fn rust_annotation_detection() {
        let source = r#"
#[test]
fn my_test() {
    assert!(true);
}

fn plain() {
    let x = 1;
}
"#;
        let parsed = ParsedFile::parse("rs", source).unwrap();

        // Line 4: inside #[test] fn my_test
        let ctx = parsed.context_at_line(4);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("my_test"));
        assert!(ctx.annotations.iter().any(|a| a.contains("test")));

        // Line 8: inside plain (no annotations)
        let ctx = parsed.context_at_line(8);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("plain"));
        assert!(ctx.annotations.is_empty());
    }

    #[test]
    fn python_decorator_detection() {
        let source = r#"
class API:
    @app.route("/users")
    def get_users(self):
        return []
"#;
        let parsed = ParsedFile::parse("py", source).unwrap();

        let ctx = parsed.context_at_line(5);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("get_users"));
        assert!(ctx.annotations.iter().any(|a| a.contains("app.route")));
    }

    #[test]
    fn java_annotation_detection() {
        let source = r#"
public class MyTest {
    @Test
    @Override
    public void testSomething() {
        assert true;
    }
}
"#;
        let parsed = ParsedFile::parse("java", source).unwrap();

        let ctx = parsed.context_at_line(6);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("testSomething"));
        assert!(ctx.annotations.iter().any(|a| a.contains("Test")));
    }

    #[test]
    fn rust_import_detection() {
        let source = r#"
use std::collections::HashMap;
use anyhow::Result;

fn main() {
    let x = 1;
}
"#;
        let parsed = ParsedFile::parse("rs", source).unwrap();

        // Line 2: import
        let ctx = parsed.context_at_line(2);
        assert!(ctx.is_import);
        assert!(ctx.is_toplevel);

        // Line 6: inside main
        let ctx = parsed.context_at_line(6);
        assert!(!ctx.is_import);
        assert!(!ctx.is_toplevel);
    }

    #[test]
    fn toplevel_and_depth() {
        let source = r#"
const X: i32 = 1;

impl Foo {
    fn method(&self) {
        let y = 2;
    }
}
"#;
        let parsed = ParsedFile::parse("rs", source).unwrap();

        // Line 2: top-level const
        let ctx = parsed.context_at_line(2);
        assert!(ctx.is_toplevel);
        assert_eq!(ctx.nesting_depth, 0);

        // Line 6: inside Foo::method (depth 2: impl + fn)
        let ctx = parsed.context_at_line(6);
        assert!(!ctx.is_toplevel);
        assert_eq!(ctx.nesting_depth, 2);
    }

    #[test]
    fn doc_comment_detection() {
        let source = r#"
/// This is a doc comment
/// for the function below.
fn documented() {
    let x = 1;
}
"#;
        let parsed = ParsedFile::parse("rs", source).unwrap();

        // Line 2: doc comment line
        let ctx = parsed.context_at_line(2);
        assert!(ctx.is_doc_comment);

        // Line 5: inside the function body
        let ctx = parsed.context_at_line(5);
        assert!(!ctx.is_doc_comment);
    }

    #[test]
    fn js_arrow_function_detection() {
        let source = r#"
const handler = () => {
    return 42;
};
"#;
        let parsed = ParsedFile::parse("js", source).unwrap();
        // Line 3: inside arrow function body — not toplevel
        let ctx = parsed.context_at_line(3);
        assert!(!ctx.is_toplevel);
        // Arrow functions are anonymous — no enclosing_function name
        assert_eq!(ctx.enclosing_function, None);
    }

    #[test]
    fn rust_import_not_doc() {
        let source = r#"
// Regular comment
use std::io;
/// Doc comment
fn foo() {}
"#;
        let parsed = ParsedFile::parse("rs", source).unwrap();

        // Line 2: regular comment — NOT a doc comment
        let ctx = parsed.context_at_line(2);
        assert!(!ctx.is_doc_comment);

        // Line 3: import
        let ctx = parsed.context_at_line(3);
        assert!(ctx.is_import);

        // Line 4: doc comment
        let ctx = parsed.context_at_line(4);
        assert!(ctx.is_doc_comment);
    }

    #[test]
    fn deep_nesting_depth() {
        let source = r#"
mod outer {
    impl Foo {
        fn deep() {
            let x = 1;
        }
    }
}
"#;
        let parsed = ParsedFile::parse("rs", source).unwrap();
        let ctx = parsed.context_at_line(5);
        assert_eq!(ctx.nesting_depth, 3); // mod + impl + fn
        assert_eq!(ctx.enclosing_function.as_deref(), Some("deep"));
    }

    #[test]
    fn elixir_function_and_module_detection() {
        let source = r#"
defmodule MyApp.Users do
  import Ecto.Query

  def get_user(id) do
    Repo.get(User, id)
  end

  defp helper do
    :ok
  end
end
"#;
        let parsed = ParsedFile::parse("ex", source).unwrap();

        // Line 6: inside MyApp.Users.get_user
        let ctx = parsed.context_at_line(6);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("get_user"));
        assert_eq!(ctx.enclosing_scope.as_deref(), Some("MyApp.Users"));
        assert!(!ctx.is_toplevel);

        // Line 10: inside helper (defp)
        let ctx = parsed.context_at_line(10);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("helper"));

        // Line 3: import line
        let ctx = parsed.context_at_line(3);
        assert!(ctx.is_import);
    }

    #[test]
    fn python_docstring_detection() {
        let source = r#"
class MyClass:
    """Class docstring."""

    def method(self):
        """Method docstring."""
        x = "not a docstring"
        return x
"#;
        let parsed = ParsedFile::parse("py", source).unwrap();

        // Line 3: class docstring
        let ctx = parsed.context_at_line(3);
        assert!(ctx.is_doc_comment);

        // Line 6: method docstring
        let ctx = parsed.context_at_line(6);
        assert!(ctx.is_doc_comment);

        // Line 7: regular string assignment — NOT a docstring
        let ctx = parsed.context_at_line(7);
        assert!(!ctx.is_doc_comment);
    }

    #[test]
    fn go_doc_comment_detection() {
        let source = r#"package main

// Hello greets the world.
// It is a doc comment.
func Hello() string {
    return "hello"
}

// just a random comment
var x = 1
"#;
        let parsed = ParsedFile::parse("go", source).unwrap();

        // Line 3: doc comment before func
        let ctx = parsed.context_at_line(3);
        assert!(ctx.is_doc_comment);

        // Line 4: also doc comment (multi-line, before func)
        let ctx = parsed.context_at_line(4);
        assert!(ctx.is_doc_comment);

        // Line 6: inside function body, not a comment
        let ctx = parsed.context_at_line(6);
        assert!(!ctx.is_doc_comment);

        // Line 9: comment before var — also a doc comment
        let ctx = parsed.context_at_line(9);
        assert!(ctx.is_doc_comment);
    }

    #[test]
    fn ruby_require_detection() {
        let source = r#"
require "json"
require_relative "helper"

class Foo
  def bar
    42
  end
end
"#;
        let parsed = ParsedFile::parse("rb", source).unwrap();

        // Line 2: require
        let ctx = parsed.context_at_line(2);
        assert!(ctx.is_import);

        // Line 3: require_relative
        let ctx = parsed.context_at_line(3);
        assert!(ctx.is_import);

        // Line 7: inside method, not an import
        let ctx = parsed.context_at_line(7);
        assert!(!ctx.is_import);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("bar"));
    }
}

#[cfg(test)]
mod toplevel_consistency_tests {
    use super::*;

    // `toplevel()` and `depth(0)` must agree, and must agree across languages.
    // Two ways they used to disagree:
    //   - Python listed "module" (the tree-sitter ROOT node) as a scope, so no
    //     Python line was ever top level and every line had depth >= 1.
    //   - A file with no language config fell back to SemanticContext::default(),
    //     which is depth 0 but is_toplevel false -- so it matched depth(0) but
    //     not toplevel().

    #[test]
    fn python_top_level_import_is_top_level() {
        let src = "import os\nimport sys\n\ndef load():\n    pass\n";
        let ctx = &contexts_for_lines("py", src, &[2])[0];
        assert!(ctx.is_import, "should be recognised as an import");
        assert!(
            ctx.is_toplevel,
            "a module-level import is top level; got is_toplevel=false"
        );
        assert_eq!(ctx.nesting_depth, 0, "module-level code is depth 0");
        assert!(ctx.enclosing_scope.is_none(), "the file itself is not a scope");
    }

    #[test]
    fn python_matches_rust_for_the_same_shape() {
        let py = &contexts_for_lines("py", "import os\nimport sys\n", &[2])[0];
        let rs = &contexts_for_lines("rs", "use std::fs;\nuse std::io;\n", &[2])[0];
        assert_eq!(
            (py.is_toplevel, py.nesting_depth),
            (rs.is_toplevel, rs.nesting_depth),
            "python and rust disagree on an equivalent top-level import"
        );
    }

    #[test]
    fn python_class_body_is_still_scoped() {
        let src = "class Store:\n    def get(self):\n        return 1\n";
        let ctx = &contexts_for_lines("py", src, &[3])[0];
        assert_eq!(ctx.enclosing_scope.as_deref(), Some("Store"));
        assert_eq!(ctx.enclosing_function.as_deref(), Some("get"));
        assert!(!ctx.is_toplevel, "code inside a class is not top level");
        assert!(ctx.nesting_depth >= 2, "class > method should nest");
    }

    #[test]
    fn python_function_body_is_not_top_level() {
        let src = "def load():\n    x = 1\n";
        let ctx = &contexts_for_lines("py", src, &[2])[0];
        assert!(!ctx.is_toplevel);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("load"));
    }

    #[test]
    fn analyzed_flag_is_set_for_supported_languages() {
        for (ext, src) in [("rs", "fn a() {}\n"), ("py", "x = 1\n"), ("go", "package m\n")] {
            let ctx = &contexts_for_lines(ext, src, &[1])[0];
            assert!(ctx.is_analyzed, "{ext} should be analyzed");
        }
    }

    #[test]
    fn analyzed_flag_is_clear_for_unsupported_languages() {
        let ctx = &contexts_for_lines("txt", "hello\nworld\n", &[1])[0];
        assert!(
            !ctx.is_analyzed,
            "a plain text file has no parser; predicates must be able to tell"
        );
        // and it must not masquerade as top-level-at-depth-0
        assert!(!ctx.is_toplevel);
    }

    #[test]
    fn no_language_config_lists_its_own_root_node_as_a_scope() {
        // A root node contains every line, so listing it as a scope makes the
        // whole file non-top-level. Guard every language against the mistake
        // Python had.
        for (ext, src) in [
            ("rs", "use std::fs;\n"),
            ("py", "import os\n"),
            ("js", "import x from 'y';\n"),
            ("go", "package m\n"),
            ("rb", "require 'x'\n"),
            ("lua", "local x = 1\n"),
            ("c", "#include <s.h>\n"),
        ] {
            let ctx = &contexts_for_lines(ext, src, &[1])[0];
            assert!(
                ctx.is_toplevel,
                "{ext}: a lone top-level statement is not top level -- \
                 does its scope_kinds include the root node?"
            );
            assert_eq!(ctx.nesting_depth, 0, "{ext}: should be depth 0");
        }
    }
}
