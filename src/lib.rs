//! Parse source code with Tree-sitter grammars and render the syntax tree as indented text.

use std::fmt::Write;
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use tree_sitter::{Language, Node, ParseOptions, ParseState, Parser, Point, Tree};

/// Languages accepted by [`parse`], sorted by name.
pub const SUPPORTED_LANGUAGES: &[&str] = &[
    "c",
    "cpp",
    "csharp",
    "go",
    "graphql",
    "hcl",
    "java",
    "javascript",
    "json",
    "jsx",
    "kotlin",
    "php",
    "prisma",
    "protobuf",
    "python",
    "ruby",
    "rust",
    "sql",
    "swift",
    "terraform",
    "thrift",
    "toml",
    "tsx",
    "typescript",
    "yaml",
];

/// How long parsing may run before it is abandoned, so pathological input cannot hang the caller.
pub const PARSE_TIMEOUT: Duration = Duration::from_secs(5);

/// The largest code [`parse`] and [`auto_detect`] accept. Parsing builds the whole tree before [`MAX_AST_BYTES`] can
/// reject it, which takes gigabytes of memory for tens of megabytes of code, and code much larger than this renders an
/// AST over that limit anyway.
pub const MAX_CODE_BYTES: usize = 4 * 1024 * 1024;

/// The largest AST [`format_ast`] renders. Real code renders about ten times its size, but indentation grows with
/// depth, so deeply nested code renders quadratically: 40 KB of nested JSON arrays would take 400 MB.
pub const MAX_AST_BYTES: usize = 64 * 1024 * 1024;

/// Languages tried by [`auto_detect`]. A grammar that also accepts another language's code comes after it; for
/// example, JSON is valid HCL, YAML, and Python, and SQL is valid Ruby. PHP can come first because a parse without
/// `<?php` is a catch-all, which [`auto_detect`] ranks last.
const DETECT_ORDER: &[&str] = &[
    "php",
    "json",
    "toml",
    "yaml",
    "graphql",
    "thrift",
    "protobuf",
    "prisma",
    "hcl",
    "sql",
    "python",
    "rust",
    "go",
    "java",
    "csharp",
    "c",
    "cpp",
    "typescript",
    "tsx",
    "swift",
    "ruby",
    "kotlin",
];

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("unsupported language `{0}`; supported languages: {list}", list = SUPPORTED_LANGUAGES.join(", "))]
    UnsupportedLanguage(String),
    #[error("the code is larger than {} MiB; parse a smaller snippet", MAX_CODE_BYTES >> 20)]
    CodeTooLarge,
    #[error("parsing did not finish within {} seconds", PARSE_TIMEOUT.as_secs())]
    Timeout,
    #[error("the AST is larger than {} MiB; parse a smaller or less deeply nested snippet", MAX_AST_BYTES >> 20)]
    AstTooLarge,
    #[error(transparent)]
    IncompatibleGrammar(#[from] tree_sitter::LanguageError),
}

/// Resolves a language name or common alias (such as `C#`, `py`, or `yml`), case-insensitively, to one of
/// [`SUPPORTED_LANGUAGES`].
pub fn resolve_language(name: &str) -> Option<&'static str> {
    let name = name.trim().to_ascii_lowercase();
    let canonical = match name.as_str() {
        "c++" | "cc" | "cxx" => "cpp",
        "c#" | "cs" | "c-sharp" => "csharp",
        "golang" => "go",
        "gql" => "graphql",
        "js" | "mjs" | "cjs" => "javascript",
        "kt" | "kts" => "kotlin",
        "proto" => "protobuf",
        "py" => "python",
        "rb" => "ruby",
        "rs" => "rust",
        "tf" => "terraform",
        "ts" | "mts" | "cts" => "typescript",
        "yml" => "yaml",
        other => other,
    };
    SUPPORTED_LANGUAGES
        .iter()
        .copied()
        .find(|&language| language == canonical)
}

fn grammar(language: &str) -> Option<Language> {
    let grammar: Language = match language {
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "csharp" => tree_sitter_c_sharp::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "graphql" => tree_sitter_graphql::LANGUAGE.into(),
        "hcl" | "terraform" => tree_sitter_hcl::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "json" => tree_sitter_json::LANGUAGE.into(),
        "kotlin" => tree_sitter_kt::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        "prisma" => tree_sitter_prisma_io::LANGUAGE.into(),
        "protobuf" => tree_sitter_proto::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "ruby" => tree_sitter_ruby::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "sql" => tree_sitter_sequel::LANGUAGE.into(),
        "swift" => tree_sitter_swift::LANGUAGE.into(),
        "thrift" => arborium_thrift::language().into(),
        "toml" => tree_sitter_toml_ng::LANGUAGE.into(),
        "jsx" | "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "javascript" | "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "yaml" => tree_sitter_yaml::LANGUAGE.into(),
        _ => return None,
    };
    Some(grammar)
}

/// Parses `code` as `language`, which may be any name or alias accepted by [`resolve_language`].
pub fn parse(language: &str, code: &str) -> Result<Tree, ParseError> {
    let grammar = resolve_language(language)
        .and_then(grammar)
        .ok_or_else(|| ParseError::UnsupportedLanguage(language.to_string()))?;
    check_code_size(code)?;
    parse_until(&grammar, code, Instant::now() + PARSE_TIMEOUT)
}

fn check_code_size(code: &str) -> Result<(), ParseError> {
    if code.len() > MAX_CODE_BYTES {
        Err(ParseError::CodeTooLarge)
    } else {
        Ok(())
    }
}

fn parse_until(grammar: &Language, code: &str, deadline: Instant) -> Result<Tree, ParseError> {
    let mut parser = Parser::new();
    parser.set_language(grammar)?;
    let bytes = code.as_bytes();
    let mut read = |offset: usize, _: Point| bytes.get(offset..).unwrap_or_default();
    let mut check_deadline = |_: &ParseState| {
        if Instant::now() < deadline {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut check_deadline);
    parser
        .parse_with_options(&mut read, None, Some(options))
        .ok_or(ParseError::Timeout)
}

/// Parses `code` with every grammar and returns the language that explains it best: a tree that is not a catch-all
/// parse, then the fewest syntax errors, then the earliest language in [`DETECT_ORDER`].
pub fn auto_detect(code: &str) -> Result<(&'static str, Tree), ParseError> {
    check_code_size(code)?;
    let deadline = Instant::now() + PARSE_TIMEOUT;
    let mut best: Option<(&'static str, Tree, (bool, usize))> = None;
    for &language in DETECT_ORDER {
        let Some(grammar) = grammar(language) else {
            continue;
        };
        let tree = parse_until(&grammar, code, deadline)?;
        let root = tree.root_node();
        let score = (is_catch_all(language, root), count_errors(root));
        if score == (false, 0) {
            return Ok((language, tree));
        }
        if best
            .as_ref()
            .is_none_or(|(_, _, best_score)| score < *best_score)
        {
            best = Some((language, tree, score));
        }
    }
    // `best` is always set because every language in `DETECT_ORDER` has a grammar.
    best.map(|(language, tree, _)| (language, tree))
        .ok_or(ParseError::Timeout)
}

/// Whether the tree is the grammar's catch-all parse of arbitrary text rather than real structure: PHP parses text
/// outside `<?php` tags as inline HTML, and YAML parses any text as a plain scalar.
fn is_catch_all(language: &str, root: Node) -> bool {
    let is_structure: fn(&str) -> bool = match language {
        "php" => |kind| kind == "php_tag",
        "yaml" => |kind| kind.ends_with("_mapping") || kind.ends_with("_sequence"),
        _ => return false,
    };
    let mut has_structure = false;
    walk_ast(root, |node, _, _| {
        has_structure |= is_structure(node.kind())
    });
    !has_structure
}

/// Calls `visit` with each node shown in the AST, its field name, and its depth, in document order. The AST shows
/// named nodes and the tokens the parser inserted to recover from errors. It leaves out anonymous nodes but not the
/// named nodes inside them, such as a comment inside Python's `is not` operator, which are shown in their place.
pub fn walk_ast<'tree>(
    root: Node<'tree>,
    mut visit: impl FnMut(Node<'tree>, Option<&'tree str>, usize),
) {
    let mut cursor = root.walk();
    let mut depth = 0;
    loop {
        let node = cursor.node();
        let is_shown = is_in_ast(node);
        if is_shown {
            visit(node, cursor.field_name(), depth);
        }
        if cursor.goto_first_child() {
            depth += usize::from(is_shown);
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return;
            }
            depth -= usize::from(is_in_ast(cursor.node()));
        }
    }
}

fn is_in_ast(node: Node) -> bool {
    node.is_named() || node.is_missing()
}

/// The node kind as shown in the AST: `ERROR` for code that could not be parsed, and `MISSING kind` for a token the
/// parser inserted to recover from an error. Anonymous tokens are quoted and escaped, so C's newline token, which
/// ends a preprocessor line, is `MISSING "\n"` rather than a line break.
pub fn node_label(node: Node) -> String {
    if !node.is_missing() {
        return node.kind().to_string();
    }
    if node.is_named() {
        format!("MISSING {}", node.kind())
    } else {
        format!("MISSING {:?}", node.kind())
    }
}

/// Counts the `ERROR` and `MISSING` nodes in the AST.
pub fn count_errors(root: Node) -> usize {
    if !root.has_error() {
        return 0;
    }
    let mut count = 0;
    walk_ast(root, |node, _, _| {
        if node.is_error() || node.is_missing() {
            count += 1;
        }
    });
    count
}

/// Renders the AST one node per line, indented two spaces per level, as `field: kind (row-column) - (row-column)`.
/// Rows and columns are zero-based, and columns count bytes. Fails if the AST is larger than [`MAX_AST_BYTES`].
pub fn format_ast(root: Node) -> Result<String, ParseError> {
    format_ast_within(root, MAX_AST_BYTES)
}

fn format_ast_within(root: Node, max_bytes: usize) -> Result<String, ParseError> {
    let mut out = String::new();
    let mut is_too_large = false;
    walk_ast(root, |node, field, depth| {
        if is_too_large {
            return;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&"  ".repeat(depth));
        if let Some(field) = field {
            out.push_str(field);
            out.push_str(": ");
        }
        out.push_str(&node_label(node));
        let (start, end) = (node.start_position(), node.end_position());
        let _ = write!(
            out,
            " ({}-{}) - ({}-{})",
            start.row, start.column, end.row, end.column
        );
        is_too_large = out.len() > max_bytes;
    });
    if is_too_large {
        Err(ParseError::AstTooLarge)
    } else {
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small, valid snippet for every supported language.
    const SAMPLES: &[(&str, &str)] = &[
        (
            "c",
            "#include <stdio.h>\n\nint main(void) {\n    printf(\"hello\\n\");\n    return 0;\n}\n",
        ),
        (
            "cpp",
            "#include <vector>\n\nnamespace app {\ntemplate <typename T>\nclass Box {\n public:\n  explicit Box(T value) : value_(value) {}\n\n private:\n  T value_;\n};\n}  // namespace app\n",
        ),
        (
            "csharp",
            "using System;\n\nnamespace App\n{\n    public class Greeter\n    {\n        public string Greet(string name) => $\"Hello, {name}\";\n    }\n}\n",
        ),
        (
            "go",
            "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(\"hello\")\n}\n",
        ),
        (
            "graphql",
            "query GetUser($id: ID!) {\n  user(id: $id) {\n    name\n    email\n  }\n}\n",
        ),
        (
            "hcl",
            "locals {\n  tags = {\n    team = \"platform\"\n  }\n}\n",
        ),
        (
            "java",
            "package app;\n\npublic class Greeter {\n    public String greet(String name) {\n        return \"Hello, \" + name;\n    }\n}\n",
        ),
        (
            "javascript",
            "import fs from \"node:fs\";\n\nexport function readConfig(path) {\n  return JSON.parse(fs.readFileSync(path, \"utf8\"));\n}\n",
        ),
        (
            "json",
            "{\"name\": \"tree-sitter-playground\", \"version\": 1, \"tags\": [\"ast\", \"web\"]}\n",
        ),
        (
            "jsx",
            "export function App({ name }) {\n  return <h1 className=\"title\">Hello, {name}</h1>;\n}\n",
        ),
        (
            "kotlin",
            "data class User(val name: String)\n\nfun greet(user: User): String = \"Hello, ${user.name}\"\n",
        ),
        (
            "php",
            "<?php\n\nfunction greet(string $name): string {\n    return \"Hello, \" . $name;\n}\n",
        ),
        (
            "prisma",
            "model User {\n  id    Int    @id @default(autoincrement())\n  email String @unique\n}\n",
        ),
        (
            "protobuf",
            "syntax = \"proto3\";\n\nmessage User {\n  string name = 1;\n  int32 id = 2;\n}\n",
        ),
        (
            "python",
            "def greet(name: str) -> str:\n    return f\"Hello, {name}\"\n",
        ),
        (
            "ruby",
            "class Greeter\n  def greet(name)\n    \"Hello, #{name}\"\n  end\nend\n",
        ),
        (
            "rust",
            "fn main() {\n    let names = vec![\"a\", \"b\"];\n    for name in &names {\n        println!(\"{name}\");\n    }\n}\n",
        ),
        (
            "sql",
            "SELECT id, email FROM users WHERE active = TRUE ORDER BY id;\n",
        ),
        (
            "swift",
            "struct User {\n    let name: String\n}\n\nfunc greet(_ user: User) -> String {\n    return \"Hello, \\(user.name)\"\n}\n",
        ),
        (
            "terraform",
            "resource \"aws_s3_bucket\" \"logs\" {\n  bucket = \"example-logs\"\n}\n\nvariable \"region\" {\n  type    = string\n  default = \"us-east-1\"\n}\n",
        ),
        (
            "thrift",
            "namespace py example\n\nstruct User {\n  1: required string name\n  2: optional i32 id\n}\n\nservice UserService {\n  User getUser(1: i32 id)\n}\n",
        ),
        (
            "toml",
            "[package]\nname = \"tree-sitter-playground\"\nversion = \"0.1.0\"\n",
        ),
        (
            "tsx",
            "export function App({ name }: { name: string }) {\n  return <h1>Hello, {name}</h1>;\n}\n",
        ),
        (
            "typescript",
            "interface User {\n  name: string;\n}\n\nexport function greet(user: User): string {\n  return `Hello, ${user.name}`;\n}\n",
        ),
        (
            "yaml",
            "name: tree-sitter-playground\ntags:\n  - ast\n  - web\n",
        ),
    ];

    #[test]
    fn test_samples_cover_every_supported_language() {
        let sampled: Vec<&str> = SAMPLES.iter().map(|(language, _)| *language).collect();
        assert_eq!(sampled, SUPPORTED_LANGUAGES);
    }

    #[test]
    fn test_every_supported_language_parses_its_sample_without_errors() {
        for (language, code) in SAMPLES {
            let tree = parse(language, code).unwrap_or_else(|error| panic!("{language}: {error}"));
            let root = tree.root_node();
            assert_eq!(
                count_errors(root),
                0,
                "{language}:\n{}",
                format_ast(root).unwrap()
            );
        }
    }

    #[test]
    fn test_auto_detect_identifies_every_sample() {
        for (language, code) in SAMPLES {
            let expected = match *language {
                "javascript" => "typescript",
                "jsx" => "tsx",
                "terraform" => "hcl",
                other => other,
            };
            let (detected, _) =
                auto_detect(code).unwrap_or_else(|error| panic!("{language}: {error}"));
            assert_eq!(detected, expected, "sample for {language}");
        }
    }

    #[test]
    fn test_auto_detect_identifies_short_snippets_dominated_by_one_string() {
        assert_eq!(
            auto_detect("{\"name\": \"tree-sitter-playground\"}")
                .unwrap()
                .0,
            "json"
        );
        assert_eq!(
            auto_detect("print(\"a fairly long greeting message\")\n")
                .unwrap()
                .0,
            "python"
        );
    }

    #[test]
    fn test_resolve_language_accepts_names_and_aliases_case_insensitively() {
        assert_eq!(resolve_language("Python"), Some("python"));
        assert_eq!(resolve_language(" C# "), Some("csharp"));
        assert_eq!(resolve_language("C++"), Some("cpp"));
        assert_eq!(resolve_language("yml"), Some("yaml"));
        assert_eq!(resolve_language("tf"), Some("terraform"));
        assert_eq!(resolve_language("cobol"), None);
    }

    #[test]
    fn test_parse_rejects_unsupported_language_with_supported_list() {
        let error = parse("cobol", "DISPLAY 'HI'.").unwrap_err();
        assert!(matches!(error, ParseError::UnsupportedLanguage(_)));
        assert!(error.to_string().contains("`cobol`"));
        assert!(error.to_string().contains("python, ruby, rust"));
    }

    #[test]
    fn test_format_ast_renders_fields_kinds_and_positions() {
        let tree = parse("python", "x = 1\n").unwrap();
        assert_eq!(
            format_ast(tree.root_node()).unwrap(),
            "module (0-0) - (1-0)\n\
             \x20 expression_statement (0-0) - (0-5)\n\
             \x20   assignment (0-0) - (0-5)\n\
             \x20     left: identifier (0-0) - (0-1)\n\
             \x20     right: integer (0-4) - (0-5)"
        );
    }

    #[test]
    fn test_format_ast_keeps_kinds_of_ancestors_of_errors() {
        let tree = parse("python", "def f():\n    x = = 1\n").unwrap();
        let ast = format_ast(tree.root_node()).unwrap();
        assert!(ast.starts_with("module (0-0)"), "{ast}");
        assert!(ast.contains("ERROR"), "{ast}");
    }

    #[test]
    fn test_format_ast_shows_missing_tokens() {
        let tree = parse("c", "int x = 1").unwrap();
        let ast = format_ast(tree.root_node()).unwrap();
        assert!(ast.contains("MISSING \";\" (0-9) - (0-9)"), "{ast}");
        assert_eq!(count_errors(tree.root_node()), 1);
    }

    #[test]
    fn test_format_ast_escapes_missing_tokens_on_one_line() {
        // The parser recovers by inserting the newline token that ends C's `#elif` line.
        let tree = parse("c", "enum e {\n#if X\nA\n#elif Y#endif\n};\n").unwrap();
        let ast = format_ast(tree.root_node()).unwrap();
        assert!(
            ast.ends_with("\n          MISSING \"\\n\" (3-7) - (3-7)"),
            "{ast}"
        );
        let mut node_count = 0;
        walk_ast(tree.root_node(), |_, _, _| node_count += 1);
        assert_eq!(ast.lines().count(), node_count, "{ast}");
    }

    #[test]
    fn test_format_ast_shows_named_nodes_inside_anonymous_nodes() {
        // Python's `is not` operator is an anonymous node, and the comment is inside it.
        let tree = parse("python", "x = (a is  # note\n     not b)\n").unwrap();
        let ast = format_ast(tree.root_node()).unwrap();
        assert!(
            ast.ends_with(
                "\n        comparison_operator (0-5) - (1-10)\n\
                 \x20         identifier (0-5) - (0-6)\n\
                 \x20         comment (0-11) - (0-17)\n\
                 \x20         identifier (1-9) - (1-10)"
            ),
            "{ast}"
        );
    }

    #[test]
    fn test_format_ast_counts_columns_in_bytes() {
        let tree = parse("python", "s = \"한글\"\n").unwrap();
        let ast = format_ast(tree.root_node()).unwrap();
        assert!(ast.contains("right: string (0-4) - (0-12)"), "{ast}");
    }

    #[test]
    fn test_format_ast_rejects_asts_larger_than_the_limit() {
        let tree = parse("python", "x = 1\n").unwrap();
        let ast = format_ast(tree.root_node()).unwrap();
        assert_eq!(format_ast_within(tree.root_node(), ast.len()).unwrap(), ast);
        let result = format_ast_within(tree.root_node(), ast.len() - 1);
        assert!(matches!(result, Err(ParseError::AstTooLarge)));
    }

    #[test]
    fn test_parse_accepts_code_up_to_the_limit() {
        let code = format!("#{}", "x".repeat(MAX_CODE_BYTES - 1));
        let tree = parse("python", &code).unwrap();
        assert_eq!(count_errors(tree.root_node()), 0);
    }

    #[test]
    fn test_parse_and_auto_detect_reject_code_larger_than_the_limit() {
        let code = "x".repeat(MAX_CODE_BYTES + 1);
        assert!(matches!(
            parse("python", &code),
            Err(ParseError::CodeTooLarge)
        ));
        assert!(matches!(auto_detect(&code), Err(ParseError::CodeTooLarge)));
    }

    #[test]
    fn test_parse_reports_unsupported_language_before_code_size() {
        let code = "x".repeat(MAX_CODE_BYTES + 1);
        assert!(matches!(
            parse("cobol", &code),
            Err(ParseError::UnsupportedLanguage(_))
        ));
    }

    #[test]
    fn test_parse_stops_at_deadline() {
        let grammar = grammar("python").unwrap();
        let code = "x = [1, 2, 3]\n".repeat(200_000);
        let result = parse_until(&grammar, &code, Instant::now());
        assert!(matches!(result, Err(ParseError::Timeout)));
    }
}
