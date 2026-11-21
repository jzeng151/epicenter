use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;
use tree_sitter::{Node, Parser};

/// Programming language classification for AST vs regex extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceLanguage {
    Python,
    TypeScript,
    Tsx,
    JavaScript,
    Fallback,
}

impl SourceLanguage {
    pub fn from_path(path: &str) -> Self {
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".py") {
            Self::Python
        } else if lower.ends_with(".tsx") {
            Self::Tsx
        } else if lower.ends_with(".ts") || lower.ends_with(".mts") || lower.ends_with(".cts") {
            Self::TypeScript
        } else if lower.ends_with(".js")
            || lower.ends_with(".jsx")
            || lower.ends_with(".mjs")
            || lower.ends_with(".cjs")
        {
            Self::JavaScript
        } else {
            Self::Fallback
        }
    }
}

/// Extracted symbol definition (function, method, class, or module scope) from a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolDef {
    pub id: String,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub is_module: bool,
    pub calls: Vec<String>,
    pub imports: Vec<String>,
}

/// Result of extracting symbols and imports from a single source file.
#[derive(Debug, Clone, Default)]
pub struct FileSymbols {
    pub file_path: String,
    pub symbols: Vec<SymbolDef>,
    pub imports: Vec<String>,
}

/// Extracts symbols, call references, and imports from `source` using `tree-sitter` for Python,
/// TypeScript/TSX, and JavaScript/JSX, with a language-agnostic regex fallback for all other files.
pub fn extract_file_symbols(file_path: &str, source: &str) -> FileSymbols {
    match SourceLanguage::from_path(file_path) {
        SourceLanguage::Python => extract_python(file_path, source),
        SourceLanguage::TypeScript => extract_ts_js(file_path, source, SourceLanguage::TypeScript),
        SourceLanguage::Tsx => extract_ts_js(file_path, source, SourceLanguage::Tsx),
        SourceLanguage::JavaScript => extract_ts_js(file_path, source, SourceLanguage::JavaScript),
        SourceLanguage::Fallback => extract_fallback(file_path, source),
    }
}

fn node_text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

fn make_symbol_id(file_path: &str, qualified_name: &str) -> String {
    format!("{file_path}::{qualified_name}")
}

fn extract_python(file_path: &str, source: &str) -> FileSymbols {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .is_err()
    {
        return extract_fallback(file_path, source);
    }
    let Some(tree) = parser.parse(source, None) else {
        return extract_fallback(file_path, source);
    };

    let root = tree.root_node();
    let mut imports = Vec::new();
    let mut symbols = Vec::new();
    let mut top_level_calls = Vec::new();

    collect_python_imports(root, source, &mut imports);
    collect_python_symbols(
        root,
        source,
        file_path,
        None,
        &imports,
        &mut symbols,
        &mut top_level_calls,
    );

    let total_lines = source.lines().count().max(1);
    if !top_level_calls.is_empty() || symbols.is_empty() {
        symbols.push(SymbolDef {
            id: make_symbol_id(file_path, "<module>"),
            name: "<module>".to_string(),
            qualified_name: "<module>".to_string(),
            file_path: file_path.to_string(),
            start_line: 1,
            end_line: total_lines,
            is_module: true,
            calls: dedup_preserve(top_level_calls),
            imports: imports.clone(),
        });
    }

    FileSymbols {
        file_path: file_path.to_string(),
        symbols,
        imports,
    }
}

fn collect_python_imports(node: Node<'_>, source: &str, imports: &mut Vec<String>) {
    match node.kind() {
        "import_statement" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "dotted_name" || child.kind() == "aliased_import" {
                    let text = node_text(child, source).trim();
                    if !text.is_empty() {
                        for seg in text.split(" as ").next().unwrap_or(text).split('.') {
                            let s = seg.trim();
                            if !s.is_empty() {
                                imports.push(s.to_string());
                            }
                        }
                    }
                }
            }
        }
        "import_from_statement" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if matches!(
                    child.kind(),
                    "dotted_name" | "identifier" | "aliased_import" | "relative_import"
                ) {
                    let text = node_text(child, source).trim();
                    let cleaned = text.split(" as ").next().unwrap_or(text);
                    for seg in cleaned.split('.') {
                        let s = seg.trim();
                        if !s.is_empty() {
                            imports.push(s.to_string());
                        }
                    }
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_python_imports(child, source, imports);
    }
}

fn collect_python_symbols(
    node: Node<'_>,
    source: &str,
    file_path: &str,
    class_ctx: Option<&str>,
    file_imports: &[String],
    symbols: &mut Vec<SymbolDef>,
    top_level_calls: &mut Vec<String>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "decorated_definition" => {
                collect_python_symbols(
                    child,
                    source,
                    file_path,
                    class_ctx,
                    file_imports,
                    symbols,
                    top_level_calls,
                );
            }
            "class_definition" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let cname = node_text(name_node, source).trim().to_string();
                    let start_line = child.start_position().row + 1;
                    let end_line = child.end_position().row + 1;
                    let mut calls = Vec::new();
                    collect_python_calls(child, source, &mut calls);
                    let qname = match class_ctx {
                        Some(parent) => format!("{parent}.{cname}"),
                        None => cname.clone(),
                    };
                    symbols.push(SymbolDef {
                        id: make_symbol_id(file_path, &qname),
                        name: cname.clone(),
                        qualified_name: qname.clone(),
                        file_path: file_path.to_string(),
                        start_line,
                        end_line,
                        is_module: false,
                        calls: dedup_preserve(calls),
                        imports: file_imports.to_vec(),
                    });
                    if let Some(body) = child.child_by_field_name("body") {
                        collect_python_symbols(
                            body,
                            source,
                            file_path,
                            Some(&qname),
                            file_imports,
                            symbols,
                            top_level_calls,
                        );
                    }
                }
            }
            "function_definition" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let fname = node_text(name_node, source).trim().to_string();
                    let start_line = child.start_position().row + 1;
                    let end_line = child.end_position().row + 1;
                    let mut calls = Vec::new();
                    if let Some(body) = child.child_by_field_name("body") {
                        collect_python_calls(body, source, &mut calls);
                    }
                    let qname = match class_ctx {
                        Some(cls) => format!("{cls}.{fname}"),
                        None => fname.clone(),
                    };
                    symbols.push(SymbolDef {
                        id: make_symbol_id(file_path, &qname),
                        name: fname,
                        qualified_name: qname,
                        file_path: file_path.to_string(),
                        start_line,
                        end_line,
                        is_module: false,
                        calls: dedup_preserve(calls),
                        imports: file_imports.to_vec(),
                    });
                }
            }
            _ => {
                if class_ctx.is_none() {
                    collect_python_calls(child, source, top_level_calls);
                }
            }
        }
    }
}

fn collect_python_calls(node: Node<'_>, source: &str, calls: &mut Vec<String>) {
    if node.kind() == "call" {
        if let Some(func) = node.child_by_field_name("function") {
            match func.kind() {
                "identifier" => {
                    let name = node_text(func, source).trim();
                    if !name.is_empty() {
                        calls.push(name.to_string());
                    }
                }
                "attribute" => {
                    if let Some(attr) = func.child_by_field_name("attribute") {
                        let attr_name = node_text(attr, source).trim();
                        if !attr_name.is_empty() {
                            calls.push(attr_name.to_string());
                        }
                    }
                    let full = node_text(func, source).trim();
                    if !full.is_empty() && full.len() <= 80 {
                        calls.push(full.to_string());
                    }
                }
                _ => {}
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "function_definition" && child.kind() != "class_definition" {
            collect_python_calls(child, source, calls);
        }
    }
}

fn extract_ts_js(file_path: &str, source: &str, lang: SourceLanguage) -> FileSymbols {
    let mut parser = Parser::new();
    let ts_lang = match lang {
        SourceLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        SourceLanguage::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        SourceLanguage::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        _ => return extract_fallback(file_path, source),
    };
    if parser.set_language(&ts_lang).is_err() {
        return extract_fallback(file_path, source);
    }
    let Some(tree) = parser.parse(source, None) else {
        return extract_fallback(file_path, source);
    };

    let root = tree.root_node();
    let mut imports = Vec::new();
    let mut symbols = Vec::new();
    let mut top_level_calls = Vec::new();

    collect_ts_imports(root, source, &mut imports);
    collect_ts_symbols(
        root,
        source,
        file_path,
        None,
        &imports,
        &mut symbols,
        &mut top_level_calls,
    );

    let total_lines = source.lines().count().max(1);
    if !top_level_calls.is_empty() || symbols.is_empty() {
        symbols.push(SymbolDef {
            id: make_symbol_id(file_path, "<module>"),
            name: "<module>".to_string(),
            qualified_name: "<module>".to_string(),
            file_path: file_path.to_string(),
            start_line: 1,
            end_line: total_lines,
            is_module: true,
            calls: dedup_preserve(top_level_calls),
            imports: imports.clone(),
        });
    }

    FileSymbols {
        file_path: file_path.to_string(),
        symbols,
        imports,
    }
}

fn collect_ts_imports(node: Node<'_>, source: &str, imports: &mut Vec<String>) {
    if node.kind() == "import_statement" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            collect_ts_import_tokens(child, source, imports);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_ts_imports(child, source, imports);
    }
}

fn collect_ts_import_tokens(node: Node<'_>, source: &str, imports: &mut Vec<String>) {
    match node.kind() {
        "identifier" | "type_identifier" => {
            let t = node_text(node, source).trim();
            if !t.is_empty() {
                imports.push(t.to_string());
            }
        }
        "string_fragment" => {
            let t = node_text(node, source).trim();
            if let Some(stem) = t.rsplit('/').next() {
                let clean = stem.trim_end_matches(".ts").trim_end_matches(".js");
                if !clean.is_empty() {
                    imports.push(clean.to_string());
                }
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                collect_ts_import_tokens(child, source, imports);
            }
        }
    }
}

fn collect_ts_symbols(
    node: Node<'_>,
    source: &str,
    file_path: &str,
    class_ctx: Option<&str>,
    file_imports: &[String],
    symbols: &mut Vec<SymbolDef>,
    top_level_calls: &mut Vec<String>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "export_statement" => {
                collect_ts_symbols(
                    child,
                    source,
                    file_path,
                    class_ctx,
                    file_imports,
                    symbols,
                    top_level_calls,
                );
            }
            "class_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let cname = node_text(name_node, source).trim().to_string();
                    let start_line = child.start_position().row + 1;
                    let end_line = child.end_position().row + 1;
                    let mut calls = Vec::new();
                    collect_ts_calls(child, source, &mut calls);
                    symbols.push(SymbolDef {
                        id: make_symbol_id(file_path, &cname),
                        name: cname.clone(),
                        qualified_name: cname.clone(),
                        file_path: file_path.to_string(),
                        start_line,
                        end_line,
                        is_module: false,
                        calls: dedup_preserve(calls),
                        imports: file_imports.to_vec(),
                    });
                    if let Some(body) = child.child_by_field_name("body") {
                        collect_ts_symbols(
                            body,
                            source,
                            file_path,
                            Some(&cname),
                            file_imports,
                            symbols,
                            top_level_calls,
                        );
                    }
                }
            }
            "function_declaration" | "generator_function_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let fname = node_text(name_node, source).trim().to_string();
                    let start_line = child.start_position().row + 1;
                    let end_line = child.end_position().row + 1;
                    let mut calls = Vec::new();
                    if let Some(body) = child.child_by_field_name("body") {
                        collect_ts_calls(body, source, &mut calls);
                    }
                    let qname = match class_ctx {
                        Some(cls) => format!("{cls}.{fname}"),
                        None => fname.clone(),
                    };
                    symbols.push(SymbolDef {
                        id: make_symbol_id(file_path, &qname),
                        name: fname,
                        qualified_name: qname,
                        file_path: file_path.to_string(),
                        start_line,
                        end_line,
                        is_module: false,
                        calls: dedup_preserve(calls),
                        imports: file_imports.to_vec(),
                    });
                }
            }
            "method_definition" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let mname = node_text(name_node, source).trim().to_string();
                    let start_line = child.start_position().row + 1;
                    let end_line = child.end_position().row + 1;
                    let mut calls = Vec::new();
                    if let Some(body) = child.child_by_field_name("body") {
                        collect_ts_calls(body, source, &mut calls);
                    }
                    let qname = match class_ctx {
                        Some(cls) => format!("{cls}.{mname}"),
                        None => mname.clone(),
                    };
                    symbols.push(SymbolDef {
                        id: make_symbol_id(file_path, &qname),
                        name: mname,
                        qualified_name: qname,
                        file_path: file_path.to_string(),
                        start_line,
                        end_line,
                        is_module: false,
                        calls: dedup_preserve(calls),
                        imports: file_imports.to_vec(),
                    });
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                let mut inner_cursor = child.walk();
                let mut found_fn = false;
                for decl in child.children(&mut inner_cursor) {
                    if decl.kind() == "variable_declarator" {
                        if let (Some(name_node), Some(val_node)) = (
                            decl.child_by_field_name("name"),
                            decl.child_by_field_name("value"),
                        ) {
                            if matches!(
                                val_node.kind(),
                                "arrow_function" | "function_expression" | "function"
                            ) {
                                found_fn = true;
                                let fname = node_text(name_node, source).trim().to_string();
                                let start_line = decl.start_position().row + 1;
                                let end_line = decl.end_position().row + 1;
                                let mut calls = Vec::new();
                                collect_ts_calls(val_node, source, &mut calls);
                                symbols.push(SymbolDef {
                                    id: make_symbol_id(file_path, &fname),
                                    name: fname.clone(),
                                    qualified_name: fname,
                                    file_path: file_path.to_string(),
                                    start_line,
                                    end_line,
                                    is_module: false,
                                    calls: dedup_preserve(calls),
                                    imports: file_imports.to_vec(),
                                });
                            }
                        }
                    }
                }
                if !found_fn && class_ctx.is_none() {
                    collect_ts_calls(child, source, top_level_calls);
                }
            }
            _ => {
                if class_ctx.is_none() {
                    collect_ts_calls(child, source, top_level_calls);
                }
            }
        }
    }
}

fn collect_ts_calls(node: Node<'_>, source: &str, calls: &mut Vec<String>) {
    match node.kind() {
        "call_expression" => {
            if let Some(func) = node.child_by_field_name("function") {
                match func.kind() {
                    "identifier" => {
                        let name = node_text(func, source).trim();
                        if !name.is_empty() {
                            calls.push(name.to_string());
                        }
                    }
                    "member_expression" => {
                        if let Some(prop) = func.child_by_field_name("property") {
                            let pname = node_text(prop, source).trim();
                            if !pname.is_empty() {
                                calls.push(pname.to_string());
                            }
                        }
                        let full = node_text(func, source).trim();
                        if !full.is_empty() && full.len() <= 80 {
                            calls.push(full.to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
        "new_expression" => {
            if let Some(ctor) = node.child_by_field_name("constructor") {
                let name = node_text(ctor, source).trim();
                if !name.is_empty() {
                    calls.push(name.to_string());
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if !matches!(
            child.kind(),
            "function_declaration" | "method_definition" | "class_declaration"
        ) {
            collect_ts_calls(child, source, calls);
        }
    }
}

/// Language-agnostic regex/identifier fallback supporting Rust, Go, Java, C#, Ruby, etc.
fn extract_fallback(file_path: &str, source: &str) -> FileSymbols {
    static DEF_RE: OnceLock<Regex> = OnceLock::new();
    static CALL_RE: OnceLock<Regex> = OnceLock::new();
    static IMPORT_RE: OnceLock<Regex> = OnceLock::new();

    let def_re = DEF_RE.get_or_init(|| {
        Regex::new(
            r"(?x)
            ^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)
            | ^\s*func\s+(?:\([^)]*\)\s+)?([A-Za-z_][A-Za-z0-9_]*)
            | ^\s*def\s+([A-Za-z_][A-Za-z0-9_]*)
            | ^\s*(?:(?:public|private|protected|internal|static|final|override|virtual|async)\s+)+(?:[A-Za-z0-9_<>\[\],?]+\s+)+([A-Za-z_][A-Za-z0-9_]*)\s*\(
            ",
        )
        .expect("valid fallback def regex")
    });
    let call_re = CALL_RE.get_or_init(|| {
        Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\(").expect("valid fallback call regex")
    });
    let import_re = IMPORT_RE.get_or_init(|| {
        Regex::new(r"^\s*(?:use|import|require|include)\s+([^\s;]+)")
            .expect("valid fallback import regex")
    });

    let keywords: HashSet<&str> = [
        "if", "else", "for", "while", "switch", "match", "return", "fn", "func", "def", "class",
        "struct", "enum", "catch", "sizeof", "typeof", "new",
    ]
    .into_iter()
    .collect();

    let lines: Vec<&str> = source.lines().collect();
    let total_lines = lines.len().max(1);
    let mut imports = Vec::new();
    let mut def_starts: Vec<(String, usize)> = Vec::new();

    for (idx, line) in lines.iter().enumerate() {
        let line_no = idx + 1;
        if let Some(cap) = import_re.captures(line) {
            if let Some(m) = cap.get(1) {
                for seg in m
                    .as_str()
                    .trim_matches('"')
                    .split(|c| c == ':' || c == '.' || c == '/')
                {
                    let s = seg.trim_matches(|c| c == '{' || c == '}' || c == ',');
                    if !s.is_empty() {
                        imports.push(s.to_string());
                    }
                }
            }
        }
        if let Some(cap) = def_re.captures(line) {
            if let Some(name) = (1..=4).find_map(|g| cap.get(g)).map(|m| m.as_str()) {
                if !keywords.contains(name) {
                    def_starts.push((name.to_string(), line_no));
                }
            }
        }
    }

    let mut symbols = Vec::new();
    for (i, (name, start_line)) in def_starts.iter().enumerate() {
        let end_line = def_starts
            .get(i + 1)
            .map(|(_, next_start)| next_start.saturating_sub(1).max(*start_line))
            .unwrap_or(total_lines);
        let body_slice = &lines[(start_line.saturating_sub(1))..end_line.min(lines.len())];
        let mut calls = Vec::new();
        for body_line in body_slice {
            for cap in call_re.captures_iter(body_line) {
                let callee = &cap[1];
                if callee != name && !keywords.contains(callee) {
                    calls.push(callee.to_string());
                }
            }
        }
        symbols.push(SymbolDef {
            id: make_symbol_id(file_path, name),
            name: name.clone(),
            qualified_name: name.clone(),
            file_path: file_path.to_string(),
            start_line: *start_line,
            end_line,
            is_module: false,
            calls: dedup_preserve(calls),
            imports: imports.clone(),
        });
    }

    if symbols.is_empty() {
        let mut calls = Vec::new();
        for line in &lines {
            for cap in call_re.captures_iter(line) {
                let callee = &cap[1];
                if !keywords.contains(callee) {
                    calls.push(callee.to_string());
                }
            }
        }
        symbols.push(SymbolDef {
            id: make_symbol_id(file_path, "<module>"),
            name: "<module>".to_string(),
            qualified_name: "<module>".to_string(),
            file_path: file_path.to_string(),
            start_line: 1,
            end_line: total_lines,
            is_module: true,
            calls: dedup_preserve(calls),
            imports: imports.clone(),
        });
    }

    FileSymbols {
        file_path: file_path.to_string(),
        symbols,
        imports,
    }
}

fn dedup_preserve(items: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if seen.insert(item.clone()) {
            out.push(item);
        }
    }
    out
}
