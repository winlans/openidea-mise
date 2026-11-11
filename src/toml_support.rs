use crate::model::{MiseTask, MiseTool, Snapshot};
use crate::{is_config_path, state};
use openidea_plugin::hover::{Hover, HoverProvider};
use openidea_plugin::indexing::{Anchor, Document as IndexDocument, DocumentIndexer, Edges};
use openidea_plugin::language_features::{
    Document as FeatureDocument, GutterAction, Injection, Injections, LanguageFeatures,
};
use openidea_plugin::providers::{
    CompletionItem, CompletionKind, Document as ProviderDocument, Providers,
};
use openidea_plugin::{Location, Position, Range, host, text};
use std::collections::BTreeMap;
use std::str::FromStr;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ParsedConfig {
    tasks: Vec<ParsedTask>,
    references: Vec<TaskReference>,
    tools: Vec<ParsedTool>,
    scripts: Vec<Range>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ParsedTask {
    name: String,
    name_range: Range,
    header_line: u32,
    description: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TaskReference {
    name: String,
    range: Range,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ParsedTool {
    name: String,
    range: Range,
}

enum Section {
    Other,
    Tools,
    Task(usize),
}

pub fn completion(
    doc: &ProviderDocument,
    at: Position,
    _trigger: Option<&str>,
) -> Vec<CompletionItem> {
    if !is_mise_document(&doc.path) {
        return Vec::new();
    }
    let parsed = parse_config(&doc.text);
    let line = line_text(&doc.text, at.line).unwrap_or_default();
    let before = line_prefix(line, at.character);
    let section = section_at_line(&doc.text, at.line);

    if is_dependency_line(before) {
        let replace = word_range(&doc.text, at);
        let inside_string = replace
            .and_then(|range| text::offset(&doc.text, range.start))
            .is_some_and(|offset| {
                offset > 0 && matches!(doc.text.as_bytes()[offset - 1], b'"' | b'\'')
            });
        return task_names(&doc.path, &parsed)
            .into_values()
            .map(|task| {
                let mut item = CompletionItem::new(&task.name, CompletionKind::Reference);
                item.detail = Some(task.description);
                item.insert_text = if inside_string {
                    task.name.clone()
                } else {
                    format!("\"{}\"", task.name)
                };
                item.replace = replace;
                item
            })
            .collect();
    }

    match section.as_deref() {
        Some("tools") => tool_name_completions(&doc.path, &doc.text, at),
        Some(section) if section.starts_with("tasks.") => task_field_completions(&doc.text, at),
        _ => top_level_completions(&doc.text, at),
    }
}

pub fn definition(doc: &ProviderDocument, at: Position) -> Vec<Location> {
    if !is_mise_document(&doc.path) {
        return Vec::new();
    }
    let parsed = parse_config(&doc.text);
    let Some(reference) = parsed
        .references
        .iter()
        .find(|reference| contains(reference.range, at))
    else {
        return Vec::new();
    };
    parsed
        .tasks
        .iter()
        .filter(|task| task.name == reference.name)
        .map(|task| Location {
            path: doc.path.clone(),
            range: task.name_range,
        })
        .collect()
}

pub fn hover(doc: &ProviderDocument, at: Position) -> Option<Hover> {
    if !is_mise_document(&doc.path) {
        return None;
    }
    let parsed = parse_config(&doc.text);
    if let Some((name, range)) = task_name_at(&parsed, at) {
        let tasks = task_names(&doc.path, &parsed);
        let task = tasks.get(name)?;
        let mut markdown = format!("**Mise task** `{}`", markdown_code(&task.name));
        if !task.description.is_empty() {
            markdown.push_str(&format!("\n\n{}", task.description));
        }
        if !task.depends.is_empty() {
            markdown.push_str(&format!(
                "\n\nDepends on: {}",
                task.depends
                    .iter()
                    .map(|dependency| format!("`{}`", markdown_code(dependency)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !task.source.is_empty() {
            markdown.push_str(&format!("\n\nSource: `{}`", markdown_code(&task.source)));
        }
        return Some(Hover::new(markdown).range(range));
    }

    let tool = parsed.tools.iter().find(|tool| contains(tool.range, at))?;
    let versions: Vec<MiseTool> = cached_snapshot(&doc.path)
        .tools
        .into_iter()
        .filter(|candidate| candidate.name == tool.name)
        .collect();
    let mut markdown = format!("**Mise tool** `{}`", markdown_code(&tool.name));
    if versions.is_empty() {
        markdown.push_str("\n\nNo cached installation information. Refresh the Mise tool window.");
    } else {
        for version in versions {
            let status = if version.active {
                "active"
            } else if version.installed {
                "installed"
            } else {
                "missing"
            };
            markdown.push_str(&format!(
                "\n\n- `{}` — {status}",
                markdown_code(&version.version)
            ));
        }
    }
    Some(Hover::new(markdown).range(tool.range))
}

pub fn gutter_actions(doc: &FeatureDocument) -> Vec<GutterAction> {
    if !is_mise_document(&doc.path) {
        return Vec::new();
    }
    parse_config(&doc.text)
        .tasks
        .into_iter()
        .map(|task| GutterAction {
            line: task.header_line,
            icon: Some("run".into()),
            tooltip: format!("Run mise task {}", task.name),
            command: "run-task".into(),
            argument: Some(task.name),
        })
        .collect()
}

pub fn injections(doc: &FeatureDocument) -> Vec<Injection> {
    if !is_mise_document(&doc.path) {
        return Vec::new();
    }
    parse_config(&doc.text)
        .scripts
        .into_iter()
        .filter(|range| !range.is_empty())
        .map(|range| Injection {
            range,
            language: "shellscript".into(),
        })
        .collect()
}

pub fn index(doc: &IndexDocument) -> Edges {
    if !is_mise_document(&doc.path) {
        return Edges::default();
    }
    let parsed = parse_config(&doc.text);
    let mut edges = Edges::default();
    edges
        .definitions
        .extend(parsed.tasks.into_iter().map(|task| Anchor {
            key: task_key(&task.name),
            range: task.name_range,
        }));
    edges
        .references
        .extend(parsed.references.into_iter().map(|reference| Anchor {
            key: task_key(&reference.name),
            range: reference.range,
        }));
    edges
}

impl Providers for crate::OpenideaMise {
    fn completion(
        &mut self,
        doc: &ProviderDocument,
        at: Position,
        trigger: Option<&str>,
    ) -> Vec<CompletionItem> {
        completion(doc, at, trigger)
    }

    fn definition(&mut self, doc: &ProviderDocument, at: Position) -> Vec<Location> {
        definition(doc, at)
    }
}

impl HoverProvider for crate::OpenideaMise {
    fn hover(&mut self, doc: &ProviderDocument, at: Position) -> Option<Hover> {
        hover(doc, at)
    }
}

impl LanguageFeatures for crate::OpenideaMise {
    fn gutter_actions(&mut self, doc: &FeatureDocument) -> Vec<GutterAction> {
        gutter_actions(doc)
    }
}

impl Injections for crate::OpenideaMise {
    fn injections(&mut self, doc: &FeatureDocument) -> Vec<Injection> {
        injections(doc)
    }
}

impl DocumentIndexer for crate::OpenideaMise {
    fn index(&mut self, doc: &IndexDocument) -> Edges {
        index(doc)
    }
}

fn parse_config(source: &str) -> ParsedConfig {
    // toml_edit is the semantic parser for complete documents. The scanner below
    // deliberately remains available when the user is halfway through an edit.
    let _semantic_document = toml_edit::DocumentMut::from_str(source).ok();
    let mut parsed = ParsedConfig::default();
    let mut section = Section::Other;
    let mut offset = 0usize;

    for raw_line in source.split_inclusive('\n') {
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let leading = line.len() - line.trim_start().len();
        let trimmed = line.trim_start();

        if let Some(header) = parse_header(source, trimmed, offset + leading) {
            section = match header {
                Header::Tools => Section::Tools,
                Header::Task { name, range } => {
                    let task = ParsedTask {
                        name,
                        name_range: range,
                        header_line: text::position(source, offset).line,
                        description: String::new(),
                    };
                    parsed.tasks.push(task);
                    Section::Task(parsed.tasks.len() - 1)
                }
                Header::Other => Section::Other,
            };
            offset += raw_line.len();
            continue;
        }

        let Some(equal) = assignment_equal(trimmed) else {
            offset += raw_line.len();
            continue;
        };
        let key_text = trimmed[..equal].trim();
        let value_relative = equal + 1;
        let value_absolute = offset + leading + value_relative;
        match section {
            Section::Tools => {
                if let Some((name, range)) = parse_key(source, key_text, offset + leading) {
                    parsed.tools.push(ParsedTool { name, range });
                }
            }
            Section::Task(index) => {
                let key = unquoted_key(key_text);
                let strings =
                    assignment_strings(source, &trimmed[value_relative..], value_absolute);
                match key.as_str() {
                    "description" => {
                        if let Some(value) = strings.first() {
                            parsed.tasks[index].description = value.value.clone();
                        }
                    }
                    "depends" | "depends_post" | "wait_for" => {
                        parsed
                            .references
                            .extend(strings.into_iter().map(|value| TaskReference {
                                name: value.value,
                                range: value.range,
                            }));
                    }
                    "run" | "script" => {
                        parsed
                            .scripts
                            .extend(strings.into_iter().map(|value| value.range));
                    }
                    _ => {}
                }
            }
            Section::Other => {}
        }
        offset += raw_line.len();
    }
    parsed
}

enum Header {
    Tools,
    Task { name: String, range: Range },
    Other,
}

fn parse_header(source: &str, trimmed: &str, absolute: usize) -> Option<Header> {
    if !trimmed.starts_with('[') || trimmed.starts_with("[[") {
        return None;
    }
    let close = trimmed.find(']')?;
    let inner = trimmed[1..close].trim();
    if inner == "tools" {
        return Some(Header::Tools);
    }
    let raw_name = inner.strip_prefix("tasks.")?.trim();
    if raw_name.is_empty() {
        return Some(Header::Other);
    }
    let name_offset = trimmed[1..close].find(raw_name)? + 1;
    if matches!(raw_name.as_bytes().first(), Some(b'"' | b'\'')) {
        let value = parse_string_at(source, absolute + name_offset)?;
        let consumed = value.end - (absolute + name_offset);
        if !raw_name[consumed..].trim().is_empty() {
            return Some(Header::Other);
        }
        return Some(Header::Task {
            name: value.value,
            range: value.range,
        });
    }
    if raw_name.contains('.') {
        return Some(Header::Other);
    }
    let start = absolute + name_offset;
    Some(Header::Task {
        name: raw_name.to_owned(),
        range: byte_range(source, start, start + raw_name.len()),
    })
}

fn parse_key(source: &str, raw: &str, absolute: usize) -> Option<(String, Range)> {
    let relative = raw.len() - raw.trim_start().len();
    let raw = raw.trim();
    if matches!(raw.as_bytes().first(), Some(b'"' | b'\'')) {
        let value = parse_string_at(source, absolute + relative)?;
        return Some((value.value, value.range));
    }
    if raw.is_empty() || raw.contains('.') {
        return None;
    }
    Some((
        raw.to_owned(),
        byte_range(source, absolute + relative, absolute + relative + raw.len()),
    ))
}

#[derive(Clone, Debug)]
struct StringValue {
    value: String,
    range: Range,
    end: usize,
}

fn assignment_strings(source: &str, line_value: &str, absolute: usize) -> Vec<StringValue> {
    let leading = line_value.len() - line_value.trim_start().len();
    let start = absolute + leading;
    let value = line_value.trim_start();
    if value.starts_with("\"\"\"") || value.starts_with("'''") {
        return parse_string_at(source, start).into_iter().collect();
    }
    scan_strings(source, value, start)
}

fn scan_strings(source: &str, value: &str, absolute: usize) -> Vec<StringValue> {
    let mut result = Vec::new();
    let mut cursor = 0usize;
    while cursor < value.len() {
        let byte = value.as_bytes()[cursor];
        if byte == b'#' {
            break;
        }
        if matches!(byte, b'"' | b'\'')
            && let Some(parsed) = parse_string_at(source, absolute + cursor)
        {
            let consumed = parsed.end.saturating_sub(absolute);
            result.push(parsed);
            cursor = consumed.max(cursor + 1);
            continue;
        }
        cursor += value[cursor..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);
    }
    result
}

fn parse_string_at(source: &str, start: usize) -> Option<StringValue> {
    let quote = *source.as_bytes().get(start)?;
    if !matches!(quote, b'"' | b'\'') {
        return None;
    }
    let triple = source
        .as_bytes()
        .get(start..start + 3)
        .is_some_and(|value| value == [quote, quote, quote]);
    let delimiter = if triple { 3 } else { 1 };
    let content_start = start + delimiter;
    let mut cursor = content_start;
    let content_end;
    let end;
    loop {
        if triple {
            if source
                .as_bytes()
                .get(cursor..cursor + 3)
                .is_some_and(|value| value == [quote, quote, quote])
            {
                content_end = cursor;
                end = cursor + 3;
                break;
            }
        } else if source.as_bytes().get(cursor) == Some(&quote) {
            content_end = cursor;
            end = cursor + 1;
            break;
        }
        let byte = *source.as_bytes().get(cursor)?;
        if quote == b'"' && byte == b'\\' {
            cursor += 1;
            let escaped = source[cursor..].chars().next()?;
            cursor += escaped.len_utf8();
        } else {
            cursor += source[cursor..].chars().next()?.len_utf8();
        }
    }
    let literal = &source[start..end];
    let value = toml_edit::Value::from_str(literal)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| source[content_start..content_end].to_owned());
    Some(StringValue {
        value,
        range: byte_range(source, content_start, content_end),
        end,
    })
}

fn assignment_equal(line: &str) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote == Some('"') {
            escaped = true;
            continue;
        }
        if matches!(character, '"' | '\'') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            }
            continue;
        }
        if character == '=' && quote.is_none() {
            return Some(index);
        }
        if character == '#' && quote.is_none() {
            return None;
        }
    }
    None
}

fn task_names(path: &str, parsed: &ParsedConfig) -> BTreeMap<String, MiseTask> {
    let mut tasks: BTreeMap<String, MiseTask> = parsed
        .tasks
        .iter()
        .map(|task| {
            (
                task.name.clone(),
                MiseTask {
                    name: task.name.clone(),
                    description: task.description.clone(),
                    ..Default::default()
                },
            )
        })
        .collect();
    for task in cached_snapshot(path).tasks {
        tasks.insert(task.name.clone(), task);
    }
    tasks
}

fn cached_snapshot(path: &str) -> Snapshot {
    let Some(project) = host::project_of(path) else {
        return Snapshot::default();
    };
    let Ok(root) = project.root() else {
        return Snapshot::default();
    };
    state::load(&root).ok().flatten().unwrap_or_default()
}

fn task_name_at(parsed: &ParsedConfig, at: Position) -> Option<(&str, Range)> {
    if let Some(task) = parsed
        .tasks
        .iter()
        .find(|task| contains(task.name_range, at))
    {
        return Some((&task.name, task.name_range));
    }
    parsed
        .references
        .iter()
        .find(|reference| contains(reference.range, at))
        .map(|reference| (reference.name.as_str(), reference.range))
}

fn tool_name_completions(path: &str, source: &str, at: Position) -> Vec<CompletionItem> {
    let replace = word_range(source, at);
    let mut tools = BTreeMap::new();
    for tool in cached_snapshot(path).tools {
        tools.entry(tool.name.clone()).or_insert(tool);
    }
    tools
        .into_values()
        .map(|tool| {
            let mut item = CompletionItem::new(&tool.name, CompletionKind::Module);
            item.detail = Some(if tool.active {
                format!("Active: {}", tool.version)
            } else {
                format!("Installed: {}", tool.version)
            });
            item.insert_text = format!("{} = \"${{1:latest}}\"", tool.name);
            item.snippet = true;
            item.replace = replace;
            item
        })
        .collect()
}

fn task_field_completions(source: &str, at: Position) -> Vec<CompletionItem> {
    const FIELDS: [(&str, &str, bool); 10] = [
        ("description", "description = \"$0\"", true),
        ("run", "run = \"$0\"", true),
        ("depends", "depends = [\"$0\"]", true),
        ("depends_post", "depends_post = [\"$0\"]", true),
        ("wait_for", "wait_for = [\"$0\"]", true),
        ("dir", "dir = \"$0\"", true),
        ("sources", "sources = [\"$0\"]", true),
        ("outputs", "outputs = [\"$0\"]", true),
        ("hide", "hide = ${1:false}", true),
        ("env", "[tasks.${1:name}.env]\n$0", true),
    ];
    let replace = word_range(source, at);
    FIELDS
        .into_iter()
        .map(|(label, insert, snippet)| {
            let mut item = CompletionItem::new(label, CompletionKind::Property);
            item.insert_text = insert.into();
            item.snippet = snippet;
            item.replace = replace;
            item
        })
        .collect()
}

fn top_level_completions(source: &str, at: Position) -> Vec<CompletionItem> {
    let replace = word_range(source, at);
    [
        (
            "Mise task",
            "[tasks.${1:name}]\ndescription = \"$2\"\nrun = \"$0\"",
        ),
        ("Mise tools", "[tools]\n${1:node} = \"$0\""),
        ("Mise environment", "[env]\n${1:NAME} = \"$0\""),
    ]
    .into_iter()
    .map(|(label, insert)| {
        let mut item = CompletionItem::new(label, CompletionKind::Snippet);
        item.insert_text = insert.into();
        item.snippet = true;
        item.replace = replace;
        item
    })
    .collect()
}

fn section_at_line(source: &str, line: u32) -> Option<String> {
    let mut current = None;
    for (index, text) in source.lines().enumerate() {
        if index as u32 > line {
            break;
        }
        let trimmed = text.trim_start();
        if trimmed.starts_with('[')
            && !trimmed.starts_with("[[")
            && let Some(close) = trimmed.find(']')
        {
            current = Some(trimmed[1..close].trim().to_owned());
        }
    }
    current
}

fn line_text(source: &str, line: u32) -> Option<&str> {
    source.lines().nth(line as usize)
}

fn line_prefix(line: &str, character: u32) -> &str {
    let mut units = 0u32;
    for (index, value) in line.char_indices() {
        let next = units + value.len_utf16() as u32;
        if next > character {
            return &line[..index];
        }
        units = next;
    }
    line
}

fn is_dependency_line(prefix: &str) -> bool {
    let key = prefix.split('=').next().map(str::trim).unwrap_or_default();
    matches!(
        unquoted_key(key).as_str(),
        "depends" | "depends_post" | "wait_for"
    )
}

fn word_range(source: &str, at: Position) -> Option<Range> {
    let offset = text::offset(source, at)?;
    let word = |character: char| {
        character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | ':' | '/' | '@')
    };
    let start = source[..offset]
        .rfind(|character: char| !word(character))
        .map_or(0, |index| index + 1);
    let end = source[offset..]
        .find(|character: char| !word(character))
        .map_or(source.len(), |index| offset + index);
    Some(byte_range(source, start, end))
}

fn unquoted_key(value: &str) -> String {
    let trimmed = value.trim();
    if let Some(value) = trimmed
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    {
        return value.to_owned();
    }
    if let Some(value) = trimmed
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
    {
        return value.to_owned();
    }
    trimmed.to_owned()
}

fn is_mise_document(path: &str) -> bool {
    is_config_path(path)
}

fn task_key(name: &str) -> String {
    format!("mise-task#{name}")
}

fn byte_range(source: &str, start: usize, end: usize) -> Range {
    Range {
        start: text::position(source, start),
        end: text::position(source, end),
    }
}

fn contains(range: Range, at: Position) -> bool {
    range.start <= at && at < range.end
}

fn markdown_code(value: &str) -> String {
    value.replace('`', "\\`")
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::indexing::Document as IndexDocument;
    use openidea_plugin::mock::MockHost;

    const CONFIG: &str = r#"[tools]
node = "22"

[tasks.lint]
description = "Check formatting"
run = "cargo fmt --check"

[tasks."build:web"]
description = "Build web"
depends = ["lint"]
wait_for = [{ task = "server" }]
run = ["pnpm install", "pnpm build"]
"#;

    fn provider_doc() -> ProviderDocument {
        ProviderDocument {
            path: "/project/mise.toml".into(),
            language: "toml".into(),
            version: 1,
            text: CONFIG.into(),
        }
    }

    #[test]
    fn parses_task_definitions_references_tools_and_scripts() {
        let parsed = parse_config(CONFIG);
        assert_eq!(
            parsed
                .tasks
                .iter()
                .map(|task| task.name.as_str())
                .collect::<Vec<_>>(),
            ["lint", "build:web"]
        );
        assert_eq!(
            parsed
                .references
                .iter()
                .map(|reference| reference.name.as_str())
                .collect::<Vec<_>>(),
            ["lint", "server"]
        );
        assert_eq!(parsed.tools[0].name, "node");
        assert_eq!(parsed.scripts.len(), 3);
        let script = parsed.scripts[0];
        assert_eq!(
            &CONFIG[text::offset(CONFIG, script.start).unwrap()
                ..text::offset(CONFIG, script.end).unwrap()],
            "cargo fmt --check"
        );
    }

    #[test]
    fn index_keys_make_rename_and_cross_file_navigation_possible() {
        let edges = index(&IndexDocument {
            path: "/project/mise.toml".into(),
            text: CONFIG.into(),
            symbols: vec![],
        });
        assert!(
            edges
                .definitions
                .iter()
                .any(|anchor| anchor.key == "mise-task#lint")
        );
        assert!(
            edges
                .references
                .iter()
                .any(|anchor| anchor.key == "mise-task#lint")
        );
        assert!(
            index(&IndexDocument {
                path: "/project/Cargo.toml".into(),
                text: CONFIG.into(),
                symbols: vec![]
            })
            .definitions
            .is_empty()
        );
    }

    #[test]
    fn definition_resolves_a_dependency_in_the_same_file() {
        let doc = provider_doc();
        let line = doc
            .text
            .lines()
            .position(|line| line.starts_with("depends"))
            .unwrap() as u32;
        let locations = definition(
            &doc,
            Position {
                line,
                character: 13,
            },
        );
        assert_eq!(locations.len(), 1);
        let start = text::offset(&doc.text, locations[0].range.start).unwrap();
        let end = text::offset(&doc.text, locations[0].range.end).unwrap();
        assert_eq!(&doc.text[start..end], "lint");
    }

    #[test]
    fn gutter_and_shell_injections_are_limited_to_mise_files() {
        let doc = FeatureDocument {
            path: "/project/mise.toml".into(),
            language: "toml".into(),
            version: 1,
            text: CONFIG.into(),
        };
        assert_eq!(gutter_actions(&doc).len(), 2);
        assert_eq!(injections(&doc).len(), 3);
        let other = FeatureDocument {
            path: "/project/Cargo.toml".into(),
            ..doc
        };
        assert!(gutter_actions(&other).is_empty());
        assert!(injections(&other).is_empty());
    }

    #[test]
    fn task_completion_uses_cached_metadata_without_running_a_process() {
        let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
        host.set_project_root(Some("/project"));
        let cached = crate::model::Snapshot {
            project: "/project".into(),
            tasks: vec![MiseTask {
                name: "cached".into(),
                description: "From refresh".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        state::save(&cached).unwrap();
        let doc = ProviderDocument {
            path: "/project/mise.toml".into(),
            language: "toml".into(),
            version: 1,
            text: "[tasks.build]\ndepends = [\"\"]".into(),
        };
        let items = completion(
            &doc,
            Position {
                line: 1,
                character: 12,
            },
            None,
        );
        assert!(items.iter().any(|item| item.label == "cached"));
    }

    #[test]
    fn ranges_are_half_open() {
        let range = Range {
            start: Position {
                line: 1,
                character: 2,
            },
            end: Position {
                line: 1,
                character: 5,
            },
        };
        assert!(contains(
            range,
            Position {
                line: 1,
                character: 2
            }
        ));
        assert!(contains(
            range,
            Position {
                line: 1,
                character: 4
            }
        ));
        assert!(!contains(
            range,
            Position {
                line: 1,
                character: 5
            }
        ));
    }

    #[test]
    fn parses_crlf_and_indented_tool_keys() {
        let parsed = parse_config("[tools]\r\n  \"node\"   = \"22\"\r\n");
        assert_eq!(parsed.tools[0].name, "node");
        let start = text::offset(
            "[tools]\r\n  \"node\"   = \"22\"\r\n",
            parsed.tools[0].range.start,
        )
        .unwrap();
        assert_eq!(start, 12);
    }
}
