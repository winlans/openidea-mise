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

    if is_dependency_line(before) || is_dependency_position(&doc.text, at) {
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
    let local: Vec<_> = parsed
        .tasks
        .iter()
        .filter(|task| task.name == reference.name)
        .map(|task| Location {
            path: doc.path.clone(),
            range: task.name_range,
        })
        .collect();
    let snapshot = cached_snapshot(&doc.path);
    let effective = snapshot
        .tasks
        .iter()
        .find(|task| task.name == reference.name);
    let Some(effective) = effective else {
        return local;
    };
    if effective.source.is_empty()
        || same_document(&effective.source, &doc.path)
        || effective
            .source
            .rsplit('/')
            .next()
            .is_some_and(|name| name == "<task>")
    {
        return local;
    }
    vec![Location {
        path: effective.source.clone(),
        range: Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 0,
                character: 0,
            },
        },
    }]
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
            if let Some(config) = cached_snapshot(&doc.path)
                .configs
                .into_iter()
                .find(|config| same_document(&config.path, &task.source))
            {
                markdown.push_str(&format!("\n\nScope: {}", markdown_code(&config.scope)));
                if config.override_file {
                    markdown.push_str(" (override)");
                } else if !config.tier.is_empty() {
                    markdown.push_str(&format!(" ({})", markdown_code(&config.tier)));
                }
            }
            if parsed
                .tasks
                .iter()
                .any(|parsed_task| parsed_task.name == task.name)
                && !same_document(&task.source, &doc.path)
            {
                markdown.push_str(
                    "\n\nThis local definition is overridden in the active Mise context.",
                );
            }
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
            if !version.requested_version.is_empty() {
                markdown.push_str(&format!(
                    " (requested `{}`)",
                    markdown_code(&version.requested_version)
                ));
            }
            if !version.source.is_empty() {
                markdown.push_str(&format!("\n  Source: `{}`", markdown_code(&version.source)));
            }
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
        let (value_start, value_end) = assignment_value_range(source, value_absolute);
        let value_text = &source[value_start..value_end];
        match section {
            Section::Tools => {
                if let Some((name, range)) = parse_key(source, key_text, offset + leading) {
                    parsed.tools.push(ParsedTool { name, range });
                }
            }
            Section::Task(index) => {
                let key = unquoted_key(key_text);
                match key.as_str() {
                    "description" => {
                        let strings = scan_strings(source, value_text, value_start);
                        if let Some(value) = strings.first() {
                            parsed.tasks[index].description = value.value.clone();
                        }
                    }
                    "depends" | "depends_post" | "wait_for" => {
                        parsed.references.extend(
                            dependency_strings(source, value_start, value_end)
                                .into_iter()
                                .map(|value| TaskReference {
                                    name: value.value,
                                    range: value.range,
                                }),
                        );
                    }
                    "run" | "script" => {
                        let strings = scan_strings(source, value_text, value_start);
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

fn assignment_value_range(source: &str, absolute: usize) -> (usize, usize) {
    let mut start = absolute.min(source.len());
    while source
        .as_bytes()
        .get(start)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        start += 1;
    }
    let Some(first) = source.as_bytes().get(start).copied() else {
        return (start, start);
    };
    if matches!(first, b'"' | b'\'') {
        return parse_string_at(source, start)
            .map(|value| (start, value.end))
            .unwrap_or((start, start));
    }
    if !matches!(first, b'[' | b'{') {
        let end = source[start..]
            .find(['\r', '\n', '#'])
            .map_or(source.len(), |relative| start + relative);
        return (start, end);
    }

    let mut stack = Vec::new();
    let mut cursor = start;
    while cursor < source.len() {
        let byte = source.as_bytes()[cursor];
        if matches!(byte, b'"' | b'\'')
            && let Some(value) = parse_string_at(source, cursor)
        {
            cursor = value.end;
            continue;
        }
        if byte == b'#' {
            cursor = source[cursor..]
                .find('\n')
                .map_or(source.len(), |relative| cursor + relative + 1);
            continue;
        }
        match byte {
            b'[' | b'{' => stack.push(byte),
            b']' if stack.last() == Some(&b'[') => {
                stack.pop();
                if stack.is_empty() {
                    return (start, cursor + 1);
                }
            }
            b'}' if stack.last() == Some(&b'{') => {
                stack.pop();
                if stack.is_empty() {
                    return (start, cursor + 1);
                }
            }
            _ => {}
        }
        cursor += source[cursor..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);
    }
    (start, source.len())
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

fn dependency_strings(source: &str, start: usize, end: usize) -> Vec<StringValue> {
    let mut result = Vec::new();
    let mut cursor = start;
    let mut object_starts = Vec::new();
    while cursor < end {
        let byte = source.as_bytes()[cursor];
        if byte == b'#' {
            cursor = source[cursor..end]
                .find('\n')
                .map_or(end, |relative| cursor + relative + 1);
            continue;
        }
        if matches!(byte, b'"' | b'\'')
            && let Some(value) = parse_string_at(source, cursor)
        {
            if value.end > end {
                break;
            }
            let after = source[value.end..end].trim_start();
            let quoted_key = after.starts_with('=');
            let accepted = if object_starts.is_empty() {
                true
            } else {
                !quoted_key
                    && inline_value_key(source, *object_starts.last().unwrap(), cursor)
                        .is_some_and(|key| matches!(key.as_str(), "task" | "name"))
            };
            if accepted {
                result.push(value.clone());
            }
            cursor = value.end;
            continue;
        }
        match byte {
            b'{' => object_starts.push(cursor),
            b'}' => {
                object_starts.pop();
            }
            _ => {}
        }
        cursor += source[cursor..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);
    }
    result
}

fn inline_value_key(source: &str, object_start: usize, value_start: usize) -> Option<String> {
    let before = &source[object_start + 1..value_start];
    let field = before
        .rsplit_once(',')
        .map_or(before, |(_, field)| field)
        .trim();
    let (key, _) = field.rsplit_once('=')?;
    Some(unquoted_key(key))
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
    let context = if host::setting_bool("followEditorContext").unwrap_or(true) {
        crate::context_directory(&root, path)
    } else {
        root.clone()
    };
    state::load(&root, &context)
        .ok()
        .flatten()
        .or_else(|| state::load(&root, &root).ok().flatten())
        .unwrap_or_default()
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
    const FIELDS: [(&str, &str, bool); 18] = [
        ("description", "description = \"$0\"", true),
        ("run", "run = \"$0\"", true),
        ("script", "script = \"$0\"", true),
        ("depends", "depends = [\"$0\"]", true),
        ("depends_post", "depends_post = [\"$0\"]", true),
        ("wait_for", "wait_for = [\"$0\"]", true),
        ("dir", "dir = \"$0\"", true),
        ("tools", "tools = { ${1:node} = \"$0\" }", true),
        ("sources", "sources = [\"$0\"]", true),
        ("outputs", "outputs = [\"$0\"]", true),
        ("shell", "shell = \"$0\"", true),
        ("file", "file = \"$0\"", true),
        ("hide", "hide = ${1:false}", true),
        ("silent", "silent = ${1:false}", true),
        ("raw", "raw = ${1:false}", true),
        ("confirm", "confirm = \"$0\"", true),
        ("alias", "alias = \"$0\"", true),
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
        ("Mise variables", "[vars]\n${1:NAME} = \"$0\""),
        ("Mise settings", "[settings]\n${1:key} = $0"),
        ("Mise hooks", "[hooks]\n${1:hook} = \"$0\""),
        ("Mise aliases", "[alias.${1:tool}]\n${2:name} = \"$0\""),
        ("Mise plugins", "[plugins]\n${1:tool} = \"$0\""),
        ("Minimum Mise version", "min_version = \"$0\""),
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

fn is_dependency_position(source: &str, at: Position) -> bool {
    let Some(cursor) = text::offset(source, at) else {
        return false;
    };
    let mut section = Section::Other;
    let mut offset = 0usize;
    let mut skip_until = 0usize;
    for raw_line in source.split_inclusive('\n') {
        if offset > cursor {
            break;
        }
        if offset < skip_until {
            offset += raw_line.len();
            continue;
        }
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let leading = line.len() - line.trim_start().len();
        let trimmed = line.trim_start();
        if let Some(header) = parse_header(source, trimmed, offset + leading) {
            section = match header {
                Header::Task { .. } => Section::Task(0),
                _ => Section::Other,
            };
            offset += raw_line.len();
            continue;
        }
        let Some(equal) = assignment_equal(trimmed) else {
            offset += raw_line.len();
            continue;
        };
        let key = unquoted_key(trimmed[..equal].trim());
        let value_absolute = offset + leading + equal + 1;
        let (start, end) = assignment_value_range(source, value_absolute);
        skip_until = end;
        if matches!(section, Section::Task(_))
            && matches!(key.as_str(), "depends" | "depends_post" | "wait_for")
            && start <= cursor
            && cursor <= end
        {
            return true;
        }
        offset += raw_line.len();
    }
    false
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

fn same_document(left: &str, right: &str) -> bool {
    left.replace('\\', "/").trim_end_matches('/') == right.replace('\\', "/").trim_end_matches('/')
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
        let _host = MockHost::install().with_capabilities(&["project.read", "storage"]);
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
    fn multiline_dependencies_only_index_task_fields_from_objects() {
        let parsed = parse_config(
            r#"[tasks.build]
depends = [
  "lint",
  { task = "server", args = ["--port", "3000"] },
  { name = "prepare", env = { TARGET = "not-a-task" } },
]
"#,
        );

        assert_eq!(
            parsed
                .references
                .iter()
                .map(|reference| reference.name.as_str())
                .collect::<Vec<_>>(),
            ["lint", "server", "prepare"]
        );
    }

    #[test]
    fn dependency_completion_works_on_multiline_array_entries() {
        let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
        host.set_project_root(Some("/project"));
        state::save(&Snapshot {
            project: "/project".into(),
            context: "/project".into(),
            tasks: vec![MiseTask {
                name: "lint".into(),
                description: "Check formatting".into(),
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
        let doc = ProviderDocument {
            path: "/project/mise.toml".into(),
            language: "toml".into(),
            version: 1,
            text: "[tasks.build]\ndepends = [\n  \"\"\n]\n".into(),
        };

        let items = completion(
            &doc,
            Position {
                line: 2,
                character: 3,
            },
            None,
        );

        assert!(items.iter().any(|item| item.label == "lint"));
    }

    #[test]
    fn definition_prefers_the_effective_inherited_task_source() {
        let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
        host.set_project_root(Some("/project"));
        state::save(&Snapshot {
            project: "/project".into(),
            context: "/project/backend".into(),
            tasks: vec![MiseTask {
                name: "lint".into(),
                source: "/project/mise.toml".into(),
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
        let doc = ProviderDocument {
            path: "/project/backend/mise.toml".into(),
            language: "toml".into(),
            version: 1,
            text: "[tasks.build]\ndepends = [\"lint\"]\n".into(),
        };

        let locations = definition(
            &doc,
            Position {
                line: 1,
                character: 13,
            },
        );

        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].path, "/project/mise.toml");
        assert_eq!(
            locations[0].range,
            Range {
                start: Position {
                    line: 0,
                    character: 0
                },
                end: Position {
                    line: 0,
                    character: 0
                }
            }
        );
    }

    #[test]
    fn hover_marks_a_local_task_overridden_by_the_effective_source() {
        let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
        host.set_project_root(Some("/project"));
        state::save(&Snapshot {
            project: "/project".into(),
            context: "/project/backend".into(),
            tasks: vec![MiseTask {
                name: "build".into(),
                description: "Effective build".into(),
                source: "/project/mise.toml".into(),
                ..Default::default()
            }],
            configs: vec![crate::model::MiseConfigFile {
                path: "/project/mise.toml".into(),
                scope: "project".into(),
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
        let doc = ProviderDocument {
            path: "/project/backend/mise.toml".into(),
            language: "toml".into(),
            version: 1,
            text: "[tasks.build]\nrun = \"echo local\"\n".into(),
        };

        let result = hover(
            &doc,
            Position {
                line: 0,
                character: 8,
            },
        )
        .unwrap();

        assert!(result.markdown.contains("Scope: project"));
        assert!(result.markdown.contains("overridden"));
        assert!(result.markdown.contains("/project/mise.toml"));
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
