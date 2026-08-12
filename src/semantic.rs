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
    /// Extra structural test a `function_kinds` match must pass. Needed where
    /// a grammar reuses one node kind for both functions and things that are
    /// not functions.
    function_filter: FunctionFilter,
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

/// Extra condition on a `function_kinds` match. A node that fails the test is
/// not a function at all: it neither names the hunk nor stops it being
/// top level.
#[derive(Clone, Copy)]
enum FunctionFilter {
    /// Every `function_kinds` match is a function.
    Any,
    /// OCaml: `let_binding` is the node kind for *all* bindings. Only bindings
    /// that take parameters -- or bind a `fun`/`function` expression -- are
    /// functions. Without this, `let top_value = 42` was a function at depth 1
    /// (so never top level) and a local `let c = ... in ...` shadowed the real
    /// enclosing function.
    OcamlLetBinding,
    /// Haskell: `function` is also the node kind for a function *type*
    /// (`Int -> Int` inside a signature), which has no `name` field -- that is
    /// why a type-signature line reported depth 1 with no name. And `bind` is
    /// both a top-level nullary definition (`konst = 42`, which should be a
    /// function) and a local `let`/`where` value binding (which should not).
    HaskellBinding,
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
    /// Zig: container types are anonymous (`struct { ... }`); the name lives on
    /// the enclosing `const Name = struct { ... };` variable declaration.
    ZigContainerName,
    /// Lua: `function f()` has a "name" field, but the module idiom
    /// `M.f = function() ... end` is an anonymous `function_definition` whose
    /// name is the assignment target.
    LuaFunctionName,
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

/// Node kinds that behave like a call, for the `call_*_targets` mechanism:
/// Elixir/Ruby emit `call`, Lua emits `function_call`, Bash emits `command`.
/// A kind that a given grammar never produces simply never matches.
const CALL_NODE_KINDS: &[&str] = &["call", "function_call", "command"];

/// Every node kind tree-sitter-javascript/typescript use for "a thing with a
/// body". `generator_function_declaration` (`function* f()`), `function_expression`
/// (`const f = function inner() {}`) and `generator_function` were missing, so
/// their bodies reported `toplevel()` at depth 0 -- an "only top-level changes"
/// selection silently swallowed them.
const JS_FUNCTION_KINDS: &[&str] = &[
    "function_declaration",
    "generator_function_declaration",
    "function_expression",
    "generator_function",
    "method_definition",
    "arrow_function",
];

/// Look up a language by file extension. `extension` must already be
/// lowercased -- `ParsedFile::parse` does that so `.RS` and `.rs` agree.
fn get_lang_config(extension: &str) -> Option<LangConfig> {
    match extension {
        "rs" => Some(LangConfig {
            language: tree_sitter_rust::LANGUAGE.into(),
            function_kinds: &["function_item"],
            scope_kinds: &["impl_item", "struct_item", "enum_item", "mod_item", "trait_item"],
            function_filter: FunctionFilter::Any,
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
        // `.pyi` stubs are Python and parse with the same grammar.
        "py" | "pyi" => Some(LangConfig {
            language: tree_sitter_python::LANGUAGE.into(),
            function_kinds: &["function_definition"],
            // NOTE: do not add "module" here. In tree-sitter-python `module`
            // is the ROOT node, so listing it as a scope makes every line in
            // the file non-top-level and depth >= 1. Ruby's "module" below is
            // a real language construct (its root is `program`), so that one
            // is correct.
            scope_kinds: &["class_definition"],
            function_filter: FunctionFilter::Any,
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
            function_kinds: JS_FUNCTION_KINDS,
            scope_kinds: &["class_declaration"],
            function_filter: FunctionFilter::Any,
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
        // `.mts`/`.cts` are the ESM/CJS TypeScript extensions; they use the
        // plain TypeScript grammar, not the TSX one.
        "ts" | "tsx" | "mts" | "cts" => {
            let language = if extension == "tsx" {
                tree_sitter_typescript::LANGUAGE_TSX.into()
            } else {
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
            };
            Some(LangConfig {
                language,
                function_kinds: JS_FUNCTION_KINDS,
                // `abstract class Foo {}` is its own node kind; without it an
                // abstract class body looked top level.
                scope_kinds: &[
                    "class_declaration",
                    "abstract_class_declaration",
                    "interface_declaration",
                ],
                function_filter: FunctionFilter::Any,
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
            // NOT `type_declaration`: it has no `name` field (the name lives on
            // its child `type_spec`/`type_alias`), so it produced a scope with
            // no name -- a struct-field hunk matched neither `scope("Point")`
            // nor `toplevel()`. Using the specs also names each entry of a
            // grouped `type ( ... )` block individually.
            scope_kinds: &["type_spec", "type_alias"],
            function_filter: FunctionFilter::Any,
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
            function_filter: FunctionFilter::Any,
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
            function_filter: FunctionFilter::Any,
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
            function_filter: FunctionFilter::Any,
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
            function_filter: FunctionFilter::Any,
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
            function_filter: FunctionFilter::Any,
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
            function_filter: FunctionFilter::Any,
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
            function_kinds: &[
                "function_declaration",
                "init_declaration",
                // methods declared in a `protocol` body have their own kind
                "protocol_function_declaration",
            ],
            // `class_declaration` covers `class`/`struct`/`enum` in this
            // grammar, but `protocol` is separate -- without it, protocol
            // bodies reported top level.
            scope_kinds: &["class_declaration", "protocol_declaration"],
            function_filter: FunctionFilter::Any,
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
            function_filter: FunctionFilter::Any,
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
            function_filter: FunctionFilter::Any,
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &[],
            comment_kinds: &["comment"],
            doc_comment_check: DocCommentCheck::None,
            // Bash has no import statement node: sourcing is an ordinary
            // `command` whose name is `source` or `.`.
            import_kinds: &[],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &["source", "."],
        }),
        "ex" | "exs" => Some(LangConfig {
            language: tree_sitter_elixir::LANGUAGE.into(),
            function_kinds: &[],
            scope_kinds: &[],
            function_filter: FunctionFilter::Any,
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
            function_filter: FunctionFilter::Any,
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &[],
            comment_kinds: &["comment"],
            doc_comment_check: DocCommentCheck::None,
            import_kinds: &["pp_include", "pp_include_lib"],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "hs" | "lhs" => Some(LangConfig {
            language: tree_sitter_haskell::LANGUAGE.into(),
            // `bind` is a nullary definition (`konst = 42`); see
            // `FunctionFilter::HaskellBinding` for why both kinds need a
            // structural test on top of the kind match.
            function_kinds: &["function", "bind"],
            scope_kinds: &["class", "data_type", "newtype"],
            function_filter: FunctionFilter::HaskellBinding,
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &[],
            // Haddock comments are their own node kind, not a prefixed
            // `comment`; matching on `-- |` text never fired.
            comment_kinds: &["comment", "haddock"],
            doc_comment_check: DocCommentCheck::NodeKind("haddock"),
            import_kinds: &["import"],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "ml" | "mli" => Some(LangConfig {
            language: tree_sitter_ocaml::LANGUAGE_OCAML.into(),
            // `let_binding` is the node kind for *every* binding -- top-level
            // values, local `let ... in`, and functions alike. The filter keeps
            // only the function-valued ones.
            function_kinds: &["let_binding"],
            scope_kinds: &["module_binding"],
            function_filter: FunctionFilter::OcamlLetBinding,
            function_name_extractor: NameExtractor::PatternField,
            // `module_binding` has no `name` field; the name is an unlabelled
            // `module_name` child.
            scope_name_extractor: NameExtractor::FirstChildOfKind("module_name"),
            annotation_kinds: &[],
            comment_kinds: &["comment"],
            doc_comment_check: DocCommentCheck::Prefixes(&["(**"]),
            // The grammar emits `open_module`; there is no `open_statement`
            // node, so `import()` never matched an OCaml `open`.
            import_kinds: &["open_module"],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "zig" => Some(LangConfig {
            language: tree_sitter_zig::LANGUAGE.into(),
            function_kinds: &["function_declaration"],
            // Container types; without these, `const Point = struct { x: i32 }`
            // field hunks reported top level.
            scope_kinds: &["struct_declaration", "enum_declaration", "union_declaration"],
            function_filter: FunctionFilter::Any,
            function_name_extractor: NameExtractor::NameField,
            scope_name_extractor: NameExtractor::ZigContainerName,
            annotation_kinds: &[],
            // The grammar emits a single `comment` kind -- there is no
            // `line_comment` or `doc_comment` node, so nothing ever matched.
            comment_kinds: &["comment"],
            doc_comment_check: DocCommentCheck::Prefixes(&["///", "//!"]),
            import_kinds: &[], // @import is a builtin_call_expression but so are @embedFile, @cImport, etc.
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &[],
        }),
        "lua" => Some(LangConfig {
            language: tree_sitter_lua::LANGUAGE.into(),
            // `function_definition` is the anonymous form used by the dominant
            // module idiom `M.f = function() ... end`; without it those bodies
            // reported top level.
            function_kinds: &["function_declaration", "function_definition"],
            scope_kinds: &[],
            function_filter: FunctionFilter::Any,
            function_name_extractor: NameExtractor::LuaFunctionName,
            scope_name_extractor: NameExtractor::NameField,
            annotation_kinds: &[],
            comment_kinds: &["comment"],
            doc_comment_check: DocCommentCheck::Prefixes(&["---"]),
            // Lua has no import statement node: `require` is an ordinary
            // `function_call`.
            import_kinds: &[],
            call_function_targets: &[],
            call_scope_targets: &[],
            call_import_targets: &["require"],
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
        // Extensions are matched case-insensitively: `.RS` and `.PY` are the
        // same languages as `.rs` and `.py`.
        let extension = extension.to_ascii_lowercase();
        let config = get_lang_config(&extension)?;
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
                let mut ctx = SemanticContext {
                    is_toplevel: true,
                    is_analyzed: true,
                    ..SemanticContext::default()
                };
                self.find_enclosing(root, line_0, &mut ctx, 0);
                ctx
            })
            .collect()
    }

    /// Walk the tree to find the innermost function and scope containing the
    /// given 0-based line.
    ///
    /// Invariant: the walk is depth-first and pre-order, so inner nodes are
    /// visited after outer ones and `enclosing_function` / `enclosing_scope`
    /// are overwritten as we descend — the last match is the innermost.
    ///
    /// The descent uses an explicit stack rather than the call stack. A syntax
    /// tree is as deep as the source nests, and generated or minified code
    /// nests far deeper than anything written by hand — a file with 50_000
    /// nested parentheses made the recursive walk abort the process with
    /// `fatal runtime error: stack overflow`. That took no bad input from the
    /// user: merely *containing* such a file was enough to crash `jj-hunk
    /// list`.
    fn find_enclosing(
        &self,
        root: tree_sitter::Node,
        line: usize,
        ctx: &mut SemanticContext,
        depth: usize,
    ) {
        let mut pending = vec![(root, depth)];
        while let Some((node, depth)) = pending.pop() {
            let depth = self.visit_node(node, line, ctx, depth);

            // Pushed in reverse so siblings pop left to right, the order the
            // recursive walk visited them in — which decides which of two
            // siblings covering the same line has the last word.
            let mut cursor = node.walk();
            let children: Vec<tree_sitter::Node> = node
                .named_children(&mut cursor)
                .filter(|child| node_contains_line(child, line))
                .collect();
            pending.extend(children.into_iter().rev().map(|child| (child, depth)));
        }
    }

    /// Fold one node into `ctx`, returning the depth its children sit at.
    fn visit_node(
        &self,
        node: tree_sitter::Node,
        line: usize,
        ctx: &mut SemanticContext,
        depth: usize,
    ) -> usize {
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

        if self.config.function_kinds.contains(&kind)
            && self.counts_as_function(&node)
            && node_contains_line(&node, line)
        {
            if let Some(name) = self.extract_function_name(&node) {
                ctx.enclosing_function = Some(name);
            }
            ctx.is_toplevel = false;
            entered_scope = true;
            self.collect_sibling_annotations(&node, ctx);
        }

        if self.config.scope_kinds.contains(&kind) && node_contains_line(&node, line) {
            if let Some(name) = self.extract_node_name(&node, self.config.scope_name_extractor) {
                ctx.enclosing_scope = Some(name);
            }
            ctx.is_toplevel = false;
            entered_scope = true;
            self.collect_sibling_annotations(&node, ctx);
        }

        // Call-based detection for languages where definitions and imports are
        // ordinary calls rather than dedicated statements: Elixir/Ruby (`call`),
        // Lua's `require` (`function_call`), Bash's `source` (`command`).
        if CALL_NODE_KINDS.contains(&kind) && node_contains_line(&node, line) {
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

        // The caller descends into NAMED children only. `children()` also
        // yields the anonymous keyword tokens, and a keyword token's `kind()`
        // *is* the keyword -- so Ruby's `class`/`module` tokens matched
        // `scope_kinds: ["class", "module"]` and were counted as a second,
        // phantom scope (same for Haskell's `newtype`/`class`). Anonymous
        // nodes are always leaves, so nothing else is lost by skipping them.
        new_depth
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
                // Must be the first named child of its parent...
                let Some(parent) = node.parent() else { return false };
                let is_first = parent
                    .named_child(0)
                    .map(|first| first.id() == node.id())
                    .unwrap_or(false);
                if !is_first {
                    return false;
                }
                // ...and that parent must be a module/function/class *body*.
                // Without the grandparent check, the first string inside any
                // `if`/`for`/`while`/`try`/`with` block counted as a docstring.
                match parent.kind() {
                    "module" => true,
                    "block" => parent
                        .parent()
                        .map(|gp| {
                            matches!(gp.kind(), "function_definition" | "class_definition")
                        })
                        .unwrap_or(false),
                    _ => false,
                }
            }
            DocCommentCheck::None => false,
        }
    }

    /// Get the callee text of a call-like node (e.g. "def", "defmodule",
    /// "require", "source"). Tries the `target` field (Elixir), then `method`
    /// (Ruby), then `name` (Lua `function_call`, Bash `command`).
    fn call_target_text(&self, node: &tree_sitter::Node) -> Option<String> {
        let target = node
            .child_by_field_name("target")
            .or_else(|| node.child_by_field_name("method"))
            .or_else(|| node.child_by_field_name("name"))?;
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
            NameExtractor::ZigContainerName => {
                let parent = node.parent()?;
                if parent.kind() != "variable_declaration" {
                    return None;
                }
                find_child_by_kind(&parent, "identifier")
                    .and_then(|n| n.utf8_text(self.source.as_bytes()).ok().map(|s| s.to_string()))
            }
            NameExtractor::LuaFunctionName => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    return name_node.utf8_text(self.source.as_bytes()).ok().map(|s| s.to_string());
                }
                // `M.f = function() ... end` parses as
                //   assignment_statement > expression_list > function_definition
                // so the name is the assignment's first target. It keeps its
                // qualifier, i.e. "M.f" rather than "f".
                let assignment = node.parent()?.parent()?;
                if assignment.kind() != "assignment_statement" {
                    return None;
                }
                let targets = find_child_by_kind(&assignment, "variable_list")?;
                targets
                    .named_child(0)?
                    .utf8_text(self.source.as_bytes())
                    .ok()
                    .map(|s| s.to_string())
            }
        }
    }

    /// Whether a node matching `function_kinds` really is a function. See
    /// `FunctionFilter` for why some grammars need this.
    fn counts_as_function(&self, node: &tree_sitter::Node) -> bool {
        match self.config.function_filter {
            FunctionFilter::Any => true,
            FunctionFilter::OcamlLetBinding => {
                find_child_by_kind(node, "parameter").is_some()
                    || node
                        .child_by_field_name("body")
                        .map(|body| {
                            matches!(body.kind(), "fun_expression" | "function_expression")
                        })
                        .unwrap_or(false)
            }
            FunctionFilter::HaskellBinding => match node.kind() {
                // A function *type* has parameter/arrow/result fields, no name.
                "function" => node.child_by_field_name("name").is_some(),
                // Only a declaration-level `bind` is a definition; one under
                // `local_binds` is a `let`/`where` value binding.
                "bind" => node
                    .parent()
                    .map(|p| {
                        matches!(
                            p.kind(),
                            "declarations" | "class_declarations" | "instance_declarations"
                        )
                    })
                    .unwrap_or(false),
                _ => true,
            },
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
    if node.start_position().row > line {
        return false;
    }
    let end = node.end_position();
    if end.row < line {
        return false;
    }
    // A node that ends at column 0 of `line` has no text on that line -- the
    // range stops just before the first character. tree-sitter-rust's
    // `line_comment` is the common case: `/// doc\n#[attr]` ends at row+1
    // col 0, which made the *attribute* line report as a doc comment.
    !(end.row == line && end.column == 0)
}

fn find_child_by_kind<'a>(node: &tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    // Bound to a local rather than returned directly: the iterator borrows
    // `cursor`, and as a tail expression its temporary outlives `cursor`'s
    // drop. The found `Node` itself borrows the tree, not the cursor.
    let found = node.children(&mut cursor).find(|child| child.kind() == kind);
    found
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

/// Regression tests for language configurations that named node kinds the
/// grammar does not actually emit, or that matched more node kinds than they
/// meant to. Each test names the symptom a user would hit.
#[cfg(test)]
mod language_config_tests {
    use super::*;

    fn ctx(ext: &str, src: &str, line: usize) -> SemanticContext {
        contexts_for_lines(ext, src, &[line]).remove(0)
    }

    // --- Go: `type_declaration` carries no `name` field ---------------------
    // The name lives on the child `type_spec`/`type_alias`, so naming
    // `type_declaration` as the scope produced a scope with no name: the hunk
    // was neither `scope("Point")` nor `toplevel()`.

    #[test]
    fn go_struct_field_is_inside_a_named_scope() {
        let src = "package main\n\ntype Point struct {\n\tX int\n}\n";
        let c = ctx("go", src, 4);
        assert_eq!(c.enclosing_scope.as_deref(), Some("Point"));
        assert!(!c.is_toplevel);
        assert_eq!(c.nesting_depth, 1);
    }

    #[test]
    fn go_grouped_type_declaration_names_each_spec() {
        let src = "package main\n\ntype (\n\tA struct{ N int }\n\tB int\n)\n";
        assert_eq!(ctx("go", src, 4).enclosing_scope.as_deref(), Some("A"));
        assert_eq!(ctx("go", src, 5).enclosing_scope.as_deref(), Some("B"));
    }

    #[test]
    fn go_type_alias_is_a_named_scope() {
        let src = "package main\n\ntype Alias = Point\n";
        assert_eq!(ctx("go", src, 3).enclosing_scope.as_deref(), Some("Alias"));
    }

    #[test]
    fn go_package_line_is_still_top_level() {
        let src = "package main\n\ntype Point struct {\n\tX int\n}\n";
        let c = ctx("go", src, 1);
        assert!(c.is_toplevel);
        assert_eq!(c.nesting_depth, 0);
    }

    // --- OCaml: `let_binding` is every binding, `module_binding` has no
    //     `name` field -------------------------------------------------------

    #[test]
    fn ocaml_top_level_value_is_top_level_not_a_function() {
        let src = "let top_value = 42\n";
        let c = ctx("ml", src, 1);
        assert_eq!(c.enclosing_function, None, "a value binding is not a function");
        assert!(c.is_toplevel);
        assert_eq!(c.nesting_depth, 0);
    }

    #[test]
    fn ocaml_local_let_does_not_shadow_the_enclosing_function() {
        let src = "let add a b =\n  let c = a + b in\n  c\n";
        assert_eq!(ctx("ml", src, 2).enclosing_function.as_deref(), Some("add"));
        assert_eq!(ctx("ml", src, 3).enclosing_function.as_deref(), Some("add"));
    }

    #[test]
    fn ocaml_module_scope_has_a_name() {
        let src = "module M = struct\n  let x = 1\nend\n";
        let c = ctx("ml", src, 2);
        assert_eq!(c.enclosing_scope.as_deref(), Some("M"));
        assert!(!c.is_toplevel);
    }

    #[test]
    fn ocaml_lambda_binding_is_a_function() {
        let src = "let f = fun x ->\n  x\n";
        assert_eq!(ctx("ml", src, 2).enclosing_function.as_deref(), Some("f"));
    }

    // --- TypeScript: `abstract class` is its own node kind ------------------

    #[test]
    fn typescript_abstract_class_is_a_scope() {
        let src = "abstract class Base {\n  abstract run(): void;\n  x = 1;\n}\n";
        let c = ctx("ts", src, 3);
        assert_eq!(c.enclosing_scope.as_deref(), Some("Base"));
        assert!(!c.is_toplevel);
    }

    #[test]
    fn tsx_abstract_class_is_a_scope() {
        let src = "abstract class Base {\n  x = 1;\n}\n";
        assert_eq!(ctx("tsx", src, 2).enclosing_scope.as_deref(), Some("Base"));
    }

    // --- False-positive toplevel(): function bodies that looked top level ---

    #[test]
    fn js_generator_body_is_not_top_level() {
        let src = "function* genFn() {\n  yield 1;\n}\n";
        let c = ctx("js", src, 2);
        assert!(!c.is_toplevel, "a generator body is not top level");
        assert_eq!(c.enclosing_function.as_deref(), Some("genFn"));
    }

    #[test]
    fn js_named_function_expression_body_is_not_top_level() {
        let src = "const f = function inner() {\n  return 2;\n};\n";
        let c = ctx("js", src, 2);
        assert!(!c.is_toplevel);
        assert_eq!(c.enclosing_function.as_deref(), Some("inner"));
    }

    #[test]
    fn js_anonymous_function_expression_body_is_not_top_level() {
        let src = "const g = function () {\n  return 1;\n};\n";
        let c = ctx("js", src, 2);
        assert!(!c.is_toplevel);
        assert_eq!(c.enclosing_function, None, "anonymous, so no name");
    }

    #[test]
    fn js_generator_expression_body_is_not_top_level() {
        let src = "const h = function* () {\n  yield 1;\n};\n";
        assert!(!ctx("js", src, 2).is_toplevel);
    }

    #[test]
    fn typescript_function_expression_body_is_not_top_level() {
        let src = "const f = function inner() {\n  return 2;\n};\n";
        assert!(!ctx("ts", src, 2).is_toplevel);
    }

    #[test]
    fn lua_assigned_function_body_is_not_top_level() {
        let src = "local M = {}\nM.f = function()\n  return 1\nend\nreturn M\n";
        let c = ctx("lua", src, 3);
        assert!(!c.is_toplevel, "`M.f = function() ... end` has a body");
        // The name comes from the assignment target, so it is qualified.
        assert_eq!(c.enclosing_function.as_deref(), Some("M.f"));
    }

    #[test]
    fn zig_struct_field_is_not_top_level() {
        let src = "const Point = struct {\n    x: i32,\n};\n";
        let c = ctx("zig", src, 2);
        assert!(!c.is_toplevel);
        assert_eq!(c.enclosing_scope.as_deref(), Some("Point"));
    }

    #[test]
    fn zig_enum_and_union_fields_are_scoped() {
        let src = "const E = enum {\n    a,\n};\nconst U = union {\n    b: i32,\n};\n";
        assert_eq!(ctx("zig", src, 2).enclosing_scope.as_deref(), Some("E"));
        assert_eq!(ctx("zig", src, 5).enclosing_scope.as_deref(), Some("U"));
    }

    #[test]
    fn zig_plain_const_is_still_top_level() {
        let src = "const x: i32 = 1;\n";
        let c = ctx("zig", src, 1);
        assert!(c.is_toplevel);
        assert_eq!(c.nesting_depth, 0);
    }

    // --- Swift: `protocol` is not a `class_declaration` ---------------------

    #[test]
    fn swift_protocol_body_is_not_top_level() {
        let src = "protocol Greeter {\n    func greet()\n}\n";
        let c = ctx("swift", src, 2);
        assert!(!c.is_toplevel);
        assert_eq!(c.enclosing_scope.as_deref(), Some("Greeter"));
    }

    #[test]
    fn swift_protocol_method_is_a_function() {
        let src = "protocol Greeter {\n    func greet()\n}\n";
        assert_eq!(ctx("swift", src, 2).enclosing_function.as_deref(), Some("greet"));
    }

    #[test]
    fn swift_struct_and_enum_still_scope() {
        let src = "struct S {\n    var x: Int\n}\n\nenum E {\n    case a\n}\n";
        assert_eq!(ctx("swift", src, 2).enclosing_scope.as_deref(), Some("S"));
        assert_eq!(ctx("swift", src, 6).enclosing_scope.as_deref(), Some("E"));
    }

    // --- Python: a docstring is the first statement of a *body* -------------

    #[test]
    fn python_string_opening_a_control_block_is_not_a_docstring() {
        for src in [
            "if True:\n    \"first\"\n",
            "for i in y:\n    \"first\"\n",
            "while True:\n    \"first\"\n",
            "with open(p) as f:\n    \"first\"\n",
            "try:\n    \"first\"\nexcept E:\n    pass\n",
        ] {
            assert!(
                !ctx("py", src, 2).is_doc_comment,
                "a bare string opening a control block is not a docstring: {src:?}"
            );
        }
    }

    #[test]
    fn python_real_docstrings_are_still_detected() {
        let src = "\"\"\"Module doc.\"\"\"\n\n\ndef f():\n    \"\"\"Fn doc.\"\"\"\n    return 1\n\n\nclass C:\n    \"\"\"Cls doc.\"\"\"\n";
        assert!(ctx("py", src, 1).is_doc_comment, "module docstring");
        assert!(ctx("py", src, 5).is_doc_comment, "function docstring");
        assert!(ctx("py", src, 10).is_doc_comment, "class docstring");
    }

    #[test]
    fn python_string_that_is_not_first_is_still_not_a_docstring() {
        let src = "def f():\n    x = 1\n    \"later\"\n";
        assert!(!ctx("py", src, 3).is_doc_comment);
    }

    // --- Ruby / Haskell: anonymous keyword tokens inflated nesting_depth ----
    // `node.children()` yields anonymous tokens too, and a keyword token's
    // `kind()` *is* the keyword, so Ruby's `class` token collided with
    // `scope_kinds: ["class", ...]` and counted as a second scope.

    #[test]
    fn ruby_class_header_depth_matches_python() {
        let rb = "class Foo\n  def bar\n    1\n  end\nend\n";
        let py = "class Foo:\n    def bar(self):\n        return 1\n";
        assert_eq!(ctx("rb", rb, 1).nesting_depth, ctx("py", py, 1).nesting_depth);
        assert_eq!(ctx("rb", rb, 1).nesting_depth, 1, "just the class");
        assert_eq!(ctx("rb", rb, 3).nesting_depth, 2, "class + method");
    }

    #[test]
    fn ruby_singleton_class_does_not_double_count() {
        let src = "class Foo\n  class << self\n    def baz\n      2\n    end\n  end\nend\n";
        assert_eq!(ctx("rb", src, 2).nesting_depth, 1, "still just `class Foo`");
    }

    #[test]
    fn ruby_module_header_depth_is_one() {
        let src = "module Foo\n  X = 1\nend\n";
        assert_eq!(ctx("rb", src, 1).nesting_depth, 1);
        assert_eq!(ctx("rb", src, 2).enclosing_scope.as_deref(), Some("Foo"));
    }

    #[test]
    fn haskell_newtype_and_class_headers_are_depth_one() {
        let src = "module M where\n\nnewtype N = N Int\n\nclass C a where\n  m :: a -> Int\n";
        assert_eq!(ctx("hs", src, 3).nesting_depth, 1, "newtype");
        assert_eq!(ctx("hs", src, 3).enclosing_scope.as_deref(), Some("N"));
        assert_eq!(ctx("hs", src, 5).nesting_depth, 1, "class");
        assert_eq!(ctx("hs", src, 5).enclosing_scope.as_deref(), Some("C"));
    }

    // --- Rust: `line_comment` extends onto the next row ---------------------

    #[test]
    fn rust_doc_comment_does_not_bleed_onto_the_next_line() {
        let src = "/// doc\n#[test]\nfn f() {\n    let x = 1;\n}\n";
        assert!(ctx("rs", src, 1).is_doc_comment, "the /// line is a doc comment");
        assert!(
            !ctx("rs", src, 2).is_doc_comment,
            "`#[test]` is an attribute, not a doc comment"
        );
        assert!(!ctx("rs", src, 3).is_doc_comment);
    }

    /// The fix for the bleeding doc comment excludes nodes that end at column 0
    /// of the queried line. Make sure that did not also drop the closing line
    /// of a block, which ends at column 1.
    #[test]
    fn closing_brace_line_is_still_inside_its_function() {
        let cases: &[(&str, &str, usize)] = &[
            ("rs", "fn f() {\n    let x = 1;\n}\n", 3),
            ("js", "function f() {\n  const x = 1;\n}\n", 3),
            ("c", "int f(void) {\n    return 0;\n}\n", 3),
            ("go", "package m\n\nfunc f() {\n\tx := 1\n}\n", 5),
            ("java", "class A {\n  void f() {\n    int x = 1;\n  }\n}\n", 4),
        ];
        for (ext, src, line) in cases {
            let c = ctx(ext, src, *line);
            assert!(!c.is_toplevel, "{ext}: the closing brace is inside the body");
            assert_eq!(c.enclosing_function.as_deref(), Some("f"), "{ext}");
        }
    }

    #[test]
    fn rust_block_doc_comment_still_works() {
        let src = "/** block */\nfn g() {}\n";
        assert!(ctx("rs", src, 1).is_doc_comment);
        assert!(!ctx("rs", src, 2).is_doc_comment);
    }

    #[test]
    fn rust_plain_comment_does_not_bleed_either() {
        let src = "// plain\n#[test]\nfn f() {}\n";
        assert!(!ctx("rs", src, 1).is_doc_comment);
        assert!(!ctx("rs", src, 2).is_doc_comment);
    }

    #[test]
    fn slash_slash_comments_in_other_languages_do_not_bleed() {
        // These already behaved; guard them against the Rust fix.
        assert!(!ctx("go", "package m\n\n// doc\nvar x = 1\n", 4).is_doc_comment);
        assert!(!ctx("java", "// doc\nclass A {}\n", 2).is_doc_comment);
        assert!(!ctx("cs", "/// doc\nclass A {}\n", 2).is_doc_comment);
        assert!(!ctx("c", "/// doc\nint f(void) { return 0; }\n", 2).is_doc_comment);
    }

    // --- Haskell / Zig: doc comments were never detected --------------------

    #[test]
    fn haskell_haddock_is_a_doc_comment() {
        let src = "module M where\n\n-- | Doc comment\nkonst :: Int\nkonst = 42\n\n-- regular\nother :: Int\nother = 1\n";
        assert!(ctx("hs", src, 3).is_doc_comment, "`-- |` is Haddock");
        assert!(!ctx("hs", src, 7).is_doc_comment, "`--` alone is not");
    }

    #[test]
    fn zig_doc_comment_is_detected() {
        let src = "/// Doc comment\n// normal\nfn f() void {}\n";
        assert!(ctx("zig", src, 1).is_doc_comment);
        assert!(!ctx("zig", src, 2).is_doc_comment);
    }

    #[test]
    fn zig_module_doc_comment_is_detected() {
        let src = "//! Module doc\nconst x: i32 = 1;\n";
        assert!(ctx("zig", src, 1).is_doc_comment);
    }

    // --- Haskell: nullary bindings and type signatures ----------------------

    #[test]
    fn haskell_nullary_binding_is_a_function() {
        let src = "module M where\n\nkonst :: Int\nkonst = 42\n";
        assert_eq!(ctx("hs", src, 4).enclosing_function.as_deref(), Some("konst"));
    }

    #[test]
    fn haskell_type_signature_is_top_level() {
        let src = "module M where\n\nfactorial :: Int -> Int\nfactorial n = n\n";
        let c = ctx("hs", src, 3);
        assert_eq!(
            c.enclosing_function, None,
            "`Int -> Int` is a function *type*, not an enclosing function"
        );
        assert!(c.is_toplevel);
        assert_eq!(c.nesting_depth, 0);
        // the equation below it is still a function
        assert_eq!(ctx("hs", src, 4).enclosing_function.as_deref(), Some("factorial"));
    }

    #[test]
    fn haskell_local_binding_does_not_shadow_the_enclosing_function() {
        let src = "module M where\n\nrun :: Int\nrun =\n  let loc = 5\n  in loc\n";
        assert_eq!(ctx("hs", src, 5).enclosing_function.as_deref(), Some("run"));
    }

    #[test]
    fn haskell_where_helper_is_still_a_function() {
        let src = "module M where\n\ntop :: Int\ntop = go 1\n  where\n    go k = k + 1\n";
        assert_eq!(ctx("hs", src, 6).enclosing_function.as_deref(), Some("go"));
    }

    // --- import(): languages whose import_kinds were silently empty ---------

    #[test]
    fn bash_source_is_an_import() {
        let src = "source ./lib.sh\n. ./other.sh\necho hi\n";
        assert!(ctx("sh", src, 1).is_import, "`source`");
        assert!(ctx("sh", src, 2).is_import, "`.`");
        assert!(!ctx("sh", src, 3).is_import, "`echo` is not an import");
    }

    #[test]
    fn lua_require_is_an_import() {
        let src = "local x = require('mod')\nlocal y = 1\n";
        assert!(ctx("lua", src, 1).is_import);
        assert!(!ctx("lua", src, 2).is_import);
    }

    #[test]
    fn erlang_include_is_an_import() {
        let src = "-module(m).\n-include(\"h.hrl\").\n-include_lib(\"x/h.hrl\").\n";
        assert!(!ctx("erl", src, 1).is_import, "-module is not an include");
        assert!(ctx("erl", src, 2).is_import);
        assert!(ctx("erl", src, 3).is_import);
    }

    // --- Extension mapping gaps --------------------------------------------

    #[test]
    fn python_stub_files_are_analyzed() {
        let c = ctx("pyi", "def f() -> int: ...\n", 1);
        assert!(c.is_analyzed);
        assert_eq!(c.enclosing_function.as_deref(), Some("f"));
    }

    #[test]
    fn typescript_module_extensions_are_analyzed() {
        for ext in ["mts", "cts"] {
            let c = ctx(ext, "export function f(): void {\n  return;\n}\n", 2);
            assert!(c.is_analyzed, "{ext} should be analyzed");
            assert_eq!(c.enclosing_function.as_deref(), Some("f"), "{ext}");
        }
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        for ext in ["RS", "Rs", "PY", "Go"] {
            assert!(
                ctx(ext, "x\n", 1).is_analyzed,
                "{ext} should map to the same config as its lowercase form"
            );
        }
        assert_eq!(
            ctx("RS", "fn f() {\n    let x = 1;\n}\n", 2).enclosing_function.as_deref(),
            Some("f")
        );
    }

    // --- cross-language regressions for the languages that already worked ---

    #[test]
    fn verified_languages_still_report_function_bodies_as_non_top_level() {
        let cases: &[(&str, &str, usize, &str)] = &[
            ("rs", "fn f() {\n    let x = 1;\n}\n", 2, "f"),
            ("py", "def f():\n    x = 1\n", 2, "f"),
            ("c", "int f(void) {\n    return 0;\n}\n", 2, "f"),
            ("cpp", "int f() {\n    return 0;\n}\n", 2, "f"),
            ("java", "class A {\n  void f() {\n    int x = 1;\n  }\n}\n", 3, "f"),
            ("cs", "class A {\n  void F() {\n    int x = 1;\n  }\n}\n", 3, "F"),
            ("scala", "object A {\n  def f(): Unit = {\n    println(1)\n  }\n}\n", 3, "f"),
            ("php", "<?php\nfunction f() {\n    return 1;\n}\n", 3, "f"),
            ("sh", "f() {\n  echo hi\n}\n", 2, "f"),
            ("go", "package m\n\nfunc f() {\n\tx := 1\n}\n", 4, "f"),
            ("rb", "def f\n  1\nend\n", 2, "f"),
            ("ex", "defmodule M do\n  def f do\n    :ok\n  end\nend\n", 3, "f"),
            ("erl", "-module(m).\nf() ->\n    ok.\n", 3, "f"),
            ("swift", "func f() {\n    let x = 1\n}\n", 2, "f"),
            ("ts", "function f(): void {\n  const x = 1;\n}\n", 2, "f"),
            ("js", "function f() {\n  const x = 1;\n}\n", 2, "f"),
            ("lua", "function f()\n  return 1\nend\n", 2, "f"),
            ("zig", "fn f() void {\n    return;\n}\n", 2, "f"),
            ("hs", "module M where\n\nf :: Int -> Int\nf n = n\n", 4, "f"),
            ("ml", "let f a =\n  a\n", 2, "f"),
        ];
        for (ext, src, line, name) in cases {
            let c = ctx(ext, src, *line);
            assert!(c.is_analyzed, "{ext}: not analyzed");
            assert!(!c.is_toplevel, "{ext}: a function body is not top level");
            assert_eq!(c.enclosing_function.as_deref(), Some(*name), "{ext}");
        }
    }

    #[test]
    fn verified_languages_still_report_file_level_statements_as_top_level() {
        let cases: &[(&str, &str, usize)] = &[
            ("rs", "use std::fs;\n", 1),
            ("py", "import os\n", 1),
            ("c", "#include <stdio.h>\n", 1),
            ("cpp", "#include <cstdio>\n", 1),
            ("java", "import java.util.List;\n", 1),
            ("cs", "using System;\n", 1),
            ("go", "package m\n", 1),
            ("js", "import x from 'y';\n", 1),
            ("ts", "import x from 'y';\n", 1),
            ("php", "<?php\nuse A\\B;\n", 2),
            ("sh", "source ./lib.sh\n", 1),
            ("lua", "local x = require('m')\n", 1),
            ("erl", "-include(\"h.hrl\").\n", 1),
            ("ml", "open List\n", 1),
            ("hs", "module M where\nimport Data.List\n", 2),
            ("zig", "const std = @import(\"std\");\n", 1),
        ];
        for (ext, src, line) in cases {
            let c = ctx(ext, src, *line);
            assert!(c.is_toplevel, "{ext}: file-level statement is top level");
            assert_eq!(c.nesting_depth, 0, "{ext}: depth 0");
        }
    }

    #[test]
    fn import_is_detected_in_every_language_that_has_one() {
        let cases: &[(&str, &str, usize)] = &[
            ("rs", "use std::fs;\n", 1),
            ("py", "import os\n", 1),
            ("c", "#include <stdio.h>\n", 1),
            ("cpp", "#include <cstdio>\n", 1),
            ("java", "import java.util.List;\n", 1),
            ("cs", "using System;\n", 1),
            ("go", "package m\n\nimport \"fmt\"\n", 3),
            ("js", "import x from 'y';\n", 1),
            ("ts", "import x from 'y';\n", 1),
            ("php", "<?php\nuse A\\B;\n", 2),
            ("scala", "import scala.util.Try\n", 1),
            ("swift", "import Foundation\n", 1),
            ("rb", "require 'json'\n", 1),
            ("ex", "defmodule M do\n  import Ecto.Query\nend\n", 2),
            ("ml", "open List\n", 1),
            ("hs", "module M where\nimport Data.List\n", 2),
            ("sh", "source ./lib.sh\n", 1),
            ("lua", "local x = require('m')\n", 1),
            ("erl", "-include(\"h.hrl\").\n", 1),
        ];
        for (ext, src, line) in cases {
            assert!(ctx(ext, src, *line).is_import, "{ext}: import not detected");
        }
    }

    // -- bug: a deep syntax tree overflowed the stack ----------------------

    /// A syntax tree is as deep as the source nests, and generated or minified
    /// code nests far deeper than anything written by hand. Walking it
    /// recursively aborted the process with `fatal runtime error: stack
    /// overflow` -- and it took no bad input from the user, only a repository
    /// that happened to contain such a file.
    ///
    /// Run on a 2 MiB stack, what a spawned thread gets by default. An
    /// overflow aborts rather than fails, which is the point: it is the crash
    /// under test and cannot be mistaken for a pass.
    #[test]
    fn a_deeply_nested_source_file_does_not_overflow_the_stack() {
        let ctx = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                let depth = 50_000;
                let source = format!(
                    "fn main() {{ let x = {}1{}; }}\n",
                    "(".repeat(depth),
                    ")".repeat(depth)
                );
                let parsed = ParsedFile::parse("rs", &source).expect("rust parser");
                parsed.context_at_line(1)
            })
            .expect("spawn")
            .join()
            .expect("walking a deep syntax tree overflowed a 2 MiB stack");

        // And it still answers correctly rather than merely surviving.
        assert_eq!(ctx.enclosing_function.as_deref(), Some("main"));
        assert!(ctx.is_analyzed);
        assert!(!ctx.is_toplevel);
    }

    /// The iterative walk must visit nodes in the same order the recursive one
    /// did -- pre-order, siblings left to right -- because the innermost
    /// match is the one that survives.
    #[test]
    fn the_innermost_enclosing_scope_still_wins() {
        let source = r#"
mod outer {
    struct Inner;
    impl Inner {
        fn deep(&self) {
            let a = 1;
        }
    }
}
"#;
        let parsed = ParsedFile::parse("rs", source).unwrap();
        let ctx = parsed.context_at_line(6);
        assert_eq!(ctx.enclosing_function.as_deref(), Some("deep"));
        assert_eq!(ctx.enclosing_scope.as_deref(), Some("Inner"));
        assert!(ctx.nesting_depth >= 2, "depth was {}", ctx.nesting_depth);
    }
}
