use crate::model::{MiseTask, MiseTool, Snapshot};
use crate::{is_config_path, is_file_task_path, state};
use openidea_plugin::diagnostics::DiagnosticsProvider;
use openidea_plugin::hover::{Hover, HoverProvider};
use openidea_plugin::indexing::{Anchor, Document as IndexDocument, DocumentIndexer, Edges};
use openidea_plugin::language_features::{
    Document as FeatureDocument, DocumentSymbol, GutterAction, Injection, Injections,
    LanguageFeatures,
};
use openidea_plugin::providers::{
    CodeAction, CompletionItem, CompletionKind, Document as ProviderDocument, Providers,
};
use openidea_plugin::semantic_tokens::{SemanticToken, SemanticTokens, TokenKind, TokenModifier};
use openidea_plugin::{Diagnostic, Location, Position, Range, Severity, TextEdit, host, text};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ParsedConfig {
    tasks: Vec<ParsedTask>,
    references: Vec<TaskReference>,
    tools: Vec<ParsedTool>,
    environment: Vec<ParsedEnvironment>,
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
    owner: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ParsedTool {
    name: String,
    range: Range,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ParsedEnvironment {
    name: String,
    range: Range,
}

enum Section {
    Other,
    Tools,
    Environment,
    Task(usize),
}

pub fn completion(
    doc: &ProviderDocument,
    at: Position,
    _trigger: Option<&str>,
) -> Vec<CompletionItem> {
    if is_mise_file_task(doc) {
        return file_task_completion(doc, at);
    }
    if !is_mise_config_provider_document(doc) {
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
        Some(section) if is_direct_task_section(section) => {
            task_field_completions(&doc.text, at, section)
        }
        _ => top_level_completions(&doc.text, at),
    }
}

pub fn definition(doc: &ProviderDocument, at: Position) -> Vec<Location> {
    if is_mise_file_task(doc) {
        return file_task_definition(doc, at);
    }
    if !is_mise_config_provider_document(doc) {
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
    if is_mise_file_task(doc) {
        return file_task_hover(doc, at);
    }
    if !is_mise_config_provider_document(doc) {
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
    if is_mise_file_task(doc) {
        let Some(name) = file_task_name(&doc.path) else {
            return Vec::new();
        };
        return vec![GutterAction {
            line: file_task_gutter_line(&doc.text),
            icon: Some("run".into()),
            tooltip: format!("Run mise task {name}"),
            command: "run-task".into(),
            argument: Some(name),
        }];
    }
    if !is_mise_config_provider_document(doc) {
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
    if !is_mise_config_provider_document(doc) {
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
    if is_mise_file_task_path(&doc.path) {
        let parsed = parse_file_task(&doc.text);
        return Edges {
            definitions: Vec::new(),
            references: parsed
                .references
                .into_iter()
                .map(|reference| Anchor {
                    key: task_key(&reference.name),
                    range: reference.range,
                })
                .collect(),
            ..Default::default()
        };
    }
    if !is_mise_config_document(&doc.path) {
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

    fn code_actions(&mut self, doc: &ProviderDocument, selection: Range) -> Vec<CodeAction> {
        code_actions(doc, selection)
    }
}

impl HoverProvider for crate::OpenideaMise {
    fn hover(&mut self, doc: &ProviderDocument, at: Position) -> Option<Hover> {
        hover(doc, at)
    }
}

impl LanguageFeatures for crate::OpenideaMise {
    fn document_symbols(&mut self, doc: &FeatureDocument) -> Vec<DocumentSymbol> {
        document_symbols(doc)
    }

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

impl DiagnosticsProvider for crate::OpenideaMise {
    fn diagnostics(&mut self, doc: &ProviderDocument) -> Vec<Diagnostic> {
        diagnostics(doc)
    }
}

impl SemanticTokens for crate::OpenideaMise {
    fn semantic_tokens(&mut self, doc: &ProviderDocument) -> Vec<SemanticToken> {
        semantic_tokens(doc)
    }
}

fn document_symbols(doc: &FeatureDocument) -> Vec<DocumentSymbol> {
    if !is_mise_config_provider_document(doc) {
        return Vec::new();
    }
    let parsed = parse_config(&doc.text);
    let document_range = byte_range(&doc.text, 0, doc.text.len());
    let mut symbols = Vec::new();

    if !parsed.tools.is_empty() {
        let parent = symbols.len() as u32;
        symbols.push(DocumentSymbol {
            name: "Tools".into(),
            detail: format!("{} configured", parsed.tools.len()),
            kind: "module".into(),
            range: document_range,
            selection_range: parsed.tools[0].range,
            parent: None,
        });
        symbols.extend(parsed.tools.into_iter().map(|tool| DocumentSymbol {
            name: tool.name,
            detail: "Mise tool".into(),
            kind: "property".into(),
            range: tool.range,
            selection_range: tool.range,
            parent: Some(parent),
        }));
    }

    if !parsed.tasks.is_empty() {
        let parent = symbols.len() as u32;
        symbols.push(DocumentSymbol {
            name: "Tasks".into(),
            detail: format!("{} configured", parsed.tasks.len()),
            kind: "module".into(),
            range: document_range,
            selection_range: parsed.tasks[0].name_range,
            parent: None,
        });
        symbols.extend(parsed.tasks.into_iter().map(|task| DocumentSymbol {
            name: task.name,
            detail: if task.description.is_empty() {
                "Mise task".into()
            } else {
                task.description
            },
            kind: "function".into(),
            range: task.name_range,
            selection_range: task.name_range,
            parent: Some(parent),
        }));
    }

    if !parsed.environment.is_empty() {
        let parent = symbols.len() as u32;
        symbols.push(DocumentSymbol {
            name: "Environment".into(),
            detail: format!("{} variables", parsed.environment.len()),
            kind: "module".into(),
            range: document_range,
            selection_range: parsed.environment[0].range,
            parent: None,
        });
        symbols.extend(
            parsed
                .environment
                .into_iter()
                .map(|environment| DocumentSymbol {
                    name: environment.name,
                    detail: "Mise environment variable".into(),
                    kind: "variable".into(),
                    range: environment.range,
                    selection_range: environment.range,
                    parent: Some(parent),
                }),
        );
    }

    symbols
}

fn diagnostics(doc: &ProviderDocument) -> Vec<Diagnostic> {
    if is_mise_file_task(doc) {
        return file_task_diagnostics(doc);
    }
    if !is_mise_config_provider_document(doc) {
        return Vec::new();
    }

    let mut diagnostics = Vec::new();
    if let Err(error) = toml_edit::DocumentMut::from_str(&doc.text) {
        let range = error
            .span()
            .map(|span| byte_range(&doc.text, span.start, span.end))
            .unwrap_or_else(|| byte_range(&doc.text, 0, 0));
        diagnostics.push(
            Diagnostic::new(range, Severity::Error, error.to_string()).code("mise.invalid-toml"),
        );
    }

    let parsed = parse_config(&doc.text);
    let mut definitions = BTreeMap::new();
    for task in &parsed.tasks {
        if definitions
            .insert(task.name.as_str(), task.name_range)
            .is_some()
        {
            diagnostics.push(
                Diagnostic::new(
                    task.name_range,
                    Severity::Error,
                    format!("Mise task `{}` is defined more than once.", task.name),
                )
                .code("mise.duplicate-task"),
            );
        }
    }

    let snapshot = cached_snapshot(&doc.path);
    let available: BTreeSet<_> = parsed
        .tasks
        .iter()
        .map(|task| task.name.as_str())
        .chain(snapshot.tasks.iter().map(|task| task.name.as_str()))
        .collect();
    let adjacency = task_adjacency(&parsed);
    for reference in &parsed.references {
        if reference.name == reference.owner {
            diagnostics.push(
                Diagnostic::new(
                    reference.range,
                    Severity::Error,
                    format!("Mise task `{}` cannot depend on itself.", reference.owner),
                )
                .code("mise.self-dependency"),
            );
        } else if !available.contains(reference.name.as_str()) {
            diagnostics.push(
                Diagnostic::new(
                    reference.range,
                    Severity::Warning,
                    format!("Mise task `{}` cannot be resolved.", reference.name),
                )
                .code("mise.unresolved-task"),
            );
        } else if dependency_closes_cycle(&reference.owner, &reference.name, &adjacency) {
            diagnostics.push(
                Diagnostic::new(
                    reference.range,
                    Severity::Error,
                    format!(
                        "Dependency `{}` creates a cycle containing `{}`.",
                        reference.name, reference.owner
                    ),
                )
                .code("mise.dependency-cycle"),
            );
        }
    }

    for task in &parsed.tasks {
        if let Some(effective) = snapshot
            .tasks
            .iter()
            .find(|effective| effective.name == task.name)
            && !effective.source.is_empty()
            && !same_document(&effective.source, &doc.path)
        {
            diagnostics.push(
                Diagnostic::new(
                    task.name_range,
                    Severity::Hint,
                    format!(
                        "This definition is overridden by the active Mise source `{}`.",
                        effective.source
                    ),
                )
                .code("mise.overridden-task"),
            );
        }
    }
    diagnostics
}

fn semantic_tokens(doc: &ProviderDocument) -> Vec<SemanticToken> {
    if is_mise_file_task(doc) {
        return file_task_semantic_tokens(doc);
    }
    if !is_mise_config_provider_document(doc) {
        return Vec::new();
    }
    let parsed = parse_config(&doc.text);
    let mut tokens = Vec::new();
    tokens.extend(parsed.tasks.into_iter().map(|task| {
        SemanticToken::new(task.name_range, TokenKind::Function).modifier(TokenModifier::Definition)
    }));
    tokens.extend(
        parsed
            .references
            .into_iter()
            .map(|reference| SemanticToken::new(reference.range, TokenKind::Function)),
    );
    tokens.extend(
        parsed
            .tools
            .into_iter()
            .map(|tool| SemanticToken::new(tool.range, TokenKind::Property)),
    );
    tokens.extend(
        parsed
            .environment
            .into_iter()
            .map(|entry| SemanticToken::new(entry.range, TokenKind::Variable)),
    );
    tokens.sort_by_key(|token| (token.range.start.line, token.range.start.character));
    tokens
}

fn task_adjacency(parsed: &ParsedConfig) -> BTreeMap<&str, Vec<&str>> {
    let mut adjacency: BTreeMap<&str, Vec<&str>> = parsed
        .tasks
        .iter()
        .map(|task| (task.name.as_str(), Vec::new()))
        .collect();
    for reference in &parsed.references {
        adjacency
            .entry(reference.owner.as_str())
            .or_default()
            .push(reference.name.as_str());
    }
    adjacency
}

fn dependency_closes_cycle(
    owner: &str,
    dependency: &str,
    adjacency: &BTreeMap<&str, Vec<&str>>,
) -> bool {
    let mut pending = vec![dependency];
    let mut visited = BTreeSet::new();
    while let Some(task) = pending.pop() {
        if task == owner {
            return true;
        }
        if !visited.insert(task) {
            continue;
        }
        if let Some(dependencies) = adjacency.get(task) {
            pending.extend(dependencies.iter().copied());
        }
    }
    false
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ParsedFileTask {
    metadata: Vec<FileTaskMetadata>,
    references: Vec<TaskReference>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FileTaskMetadata {
    key: String,
    key_range: Range,
}

const FILE_TASK_OPTIONS: [(&str, &str, &str); 14] = [
    ("alias", "string | string[]", "Alias for this task"),
    (
        "depends",
        "string | string[]",
        "Tasks to run before this task",
    ),
    (
        "depends_post",
        "string | string[]",
        "Tasks to run after this task",
    ),
    (
        "wait_for",
        "string | string[]",
        "Tasks whose completion this task waits for",
    ),
    ("description", "string", "Description of this task"),
    ("dir", "string", "Working directory for this task"),
    ("env", "table", "Environment variables for this task"),
    ("tools", "table", "Tools activated for this task"),
    ("hide", "bool", "Hide this task from listings"),
    ("outputs", "string | string[]", "Files created by this task"),
    ("quiet", "bool", "Hide Mise status output"),
    ("sources", "string | string[]", "Input files for this task"),
    ("shell", "string", "Shell used to run this task"),
    ("usage", "string", "Usage specification for task arguments"),
];

fn parse_file_task(source: &str) -> ParsedFileTask {
    let mut parsed = ParsedFileTask::default();
    let mut offset = 0usize;
    for raw_line in source.split_inclusive('\n') {
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let leading = line.len() - line.trim_start().len();
        let trimmed = line.trim_start();
        let Some(metadata) = trimmed.strip_prefix("#MISE ") else {
            offset += raw_line.len();
            continue;
        };
        let Some(equal) = assignment_equal(metadata) else {
            offset += raw_line.len();
            continue;
        };
        let raw_key = metadata[..equal].trim();
        if raw_key.is_empty() {
            offset += raw_line.len();
            continue;
        }
        let metadata_absolute = offset + leading + "#MISE ".len();
        let key_leading = metadata[..equal].len() - metadata[..equal].trim_start().len();
        let key_start = metadata_absolute + key_leading;
        let value_start = metadata_absolute + equal + 1;
        let value_end = offset + line.len();
        let key = unquoted_key(raw_key);
        parsed.metadata.push(FileTaskMetadata {
            key: key.clone(),
            key_range: byte_range(source, key_start, key_start + raw_key.len()),
        });
        if matches!(key.as_str(), "depends" | "depends_post" | "wait_for") {
            parsed.references.extend(
                dependency_strings(source, value_start.min(value_end), value_end)
                    .into_iter()
                    .map(|value| TaskReference {
                        name: value.value,
                        range: value.range,
                        owner: String::new(),
                    }),
            );
        }
        offset += raw_line.len();
    }
    parsed
}

fn file_task_completion(doc: &ProviderDocument, at: Position) -> Vec<CompletionItem> {
    let line = line_text(&doc.text, at.line).unwrap_or_default();
    let prefix = line_prefix(line, at.character);
    let Some(metadata) = prefix.trim_start().strip_prefix("#MISE ") else {
        return Vec::new();
    };
    let key = metadata
        .split_once('=')
        .map(|(key, _)| unquoted_key(key))
        .unwrap_or_default();
    if matches!(key.as_str(), "depends" | "depends_post" | "wait_for") && metadata.contains('=') {
        let replace = word_range(&doc.text, at);
        let inside_string = replace
            .and_then(|range| text::offset(&doc.text, range.start))
            .is_some_and(|offset| {
                offset > 0 && matches!(doc.text.as_bytes()[offset - 1], b'"' | b'\'')
            });
        return task_names(&doc.path, &ParsedConfig::default())
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
    let replace = word_range(&doc.text, at);
    FILE_TASK_OPTIONS
        .into_iter()
        .map(|(name, value_type, description)| {
            let mut item = CompletionItem::new(name, CompletionKind::Property);
            item.detail = Some(format!("{value_type} — {description}"));
            item.insert_text = name.into();
            item.replace = replace;
            item
        })
        .collect()
}

fn file_task_definition(doc: &ProviderDocument, at: Position) -> Vec<Location> {
    let parsed = parse_file_task(&doc.text);
    let Some(reference) = parsed
        .references
        .iter()
        .find(|reference| contains(reference.range, at))
    else {
        return Vec::new();
    };
    cached_snapshot(&doc.path)
        .tasks
        .into_iter()
        .find(|task| task.name == reference.name && !task.source.is_empty())
        .map(|task| {
            vec![Location {
                path: task.source,
                range: Range::default(),
            }]
        })
        .unwrap_or_default()
}

fn file_task_hover(doc: &ProviderDocument, at: Position) -> Option<Hover> {
    let parsed = parse_file_task(&doc.text);
    if let Some(reference) = parsed
        .references
        .iter()
        .find(|reference| contains(reference.range, at))
    {
        let task = task_names(&doc.path, &ParsedConfig::default()).remove(&reference.name)?;
        let mut markdown = format!("**Mise task** `{}`", markdown_code(&task.name));
        if !task.description.is_empty() {
            markdown.push_str(&format!("\n\n{}", task.description));
        }
        if !task.source.is_empty() {
            markdown.push_str(&format!("\n\nSource: `{}`", markdown_code(&task.source)));
        }
        return Some(Hover::new(markdown).range(reference.range));
    }
    if let Some(metadata) = parsed
        .metadata
        .iter()
        .find(|metadata| contains(metadata.key_range, at))
        && let Some((_, value_type, description)) = FILE_TASK_OPTIONS
            .iter()
            .find(|(name, _, _)| *name == metadata.key)
    {
        return Some(
            Hover::new(format!(
                "**Mise File Task metadata** `{}`\n\n{}\n\nType: `{}`",
                markdown_code(&metadata.key),
                description,
                markdown_code(value_type)
            ))
            .range(metadata.key_range),
        );
    }
    let name = file_task_name(&doc.path)?;
    Some(Hover::new(format!(
        "**Mise File Task** `{}`\n\nSource: `{}`",
        markdown_code(&name),
        markdown_code(&doc.path)
    )))
}

fn file_task_diagnostics(doc: &ProviderDocument) -> Vec<Diagnostic> {
    let parsed = parse_file_task(&doc.text);
    let name = file_task_name(&doc.path).unwrap_or_default();
    let snapshot = cached_snapshot(&doc.path);
    let has_task_catalog = !snapshot.tasks.is_empty();
    let available: BTreeSet<_> = snapshot.tasks.into_iter().map(|task| task.name).collect();
    let known: BTreeSet<_> = FILE_TASK_OPTIONS.iter().map(|(name, _, _)| *name).collect();
    let mut diagnostics = Vec::new();
    for metadata in &parsed.metadata {
        if !known.contains(metadata.key.as_str()) {
            diagnostics.push(
                Diagnostic::new(
                    metadata.key_range,
                    Severity::Warning,
                    format!("Unknown Mise File Task metadata `{}`.", metadata.key),
                )
                .code("mise.unknown-file-task-metadata"),
            );
        }
    }
    for reference in parsed.references {
        if reference.name == name {
            diagnostics.push(
                Diagnostic::new(
                    reference.range,
                    Severity::Error,
                    format!("Mise task `{name}` cannot depend on itself."),
                )
                .code("mise.self-dependency"),
            );
        } else if has_task_catalog && !available.contains(&reference.name) {
            diagnostics.push(
                Diagnostic::new(
                    reference.range,
                    Severity::Warning,
                    format!("Mise task `{}` cannot be resolved.", reference.name),
                )
                .code("mise.unresolved-task"),
            );
        }
    }
    diagnostics
}

fn file_task_semantic_tokens(doc: &ProviderDocument) -> Vec<SemanticToken> {
    let parsed = parse_file_task(&doc.text);
    let mut tokens: Vec<_> = parsed
        .metadata
        .into_iter()
        .map(|metadata| SemanticToken::new(metadata.key_range, TokenKind::Property))
        .chain(
            parsed
                .references
                .into_iter()
                .map(|reference| SemanticToken::new(reference.range, TokenKind::Function)),
        )
        .collect();
    tokens.sort_by_key(|token| (token.range.start.line, token.range.start.character));
    tokens
}

fn file_task_gutter_line(source: &str) -> u32 {
    source
        .lines()
        .position(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with("#!") && !line.starts_with("#MISE ")
        })
        .unwrap_or(0) as u32
}

fn is_mise_file_task(doc: &ProviderDocument) -> bool {
    doc.language == "shellscript"
        && !is_mise_config_document(&doc.path)
        && (is_mise_file_task_path(&doc.path)
            || cached_snapshot(&doc.path)
                .tasks
                .iter()
                .any(|task| same_document(&task.source, &doc.path)))
}

fn is_mise_file_task_path(path: &str) -> bool {
    is_file_task_path(path)
}

fn file_task_name(path: &str) -> Option<String> {
    if let Some(task) = cached_snapshot(path)
        .tasks
        .into_iter()
        .find(|task| same_document(&task.source, path))
    {
        return Some(task.name);
    }
    crate::file_task_name(path)
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
                Header::Environment => Section::Environment,
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
            Section::Environment => {
                if let Some((name, range)) = parse_key(source, key_text, offset + leading) {
                    parsed.environment.push(ParsedEnvironment { name, range });
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
                                    owner: parsed.tasks[index].name.clone(),
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
    Environment,
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
    if inner == "env" {
        return Some(Header::Environment);
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

fn task_field_completions(source: &str, at: Position, task_section: &str) -> Vec<CompletionItem> {
    const FIELDS: [(&str, &str, bool); 17] = [
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
    ];
    let replace = word_range(source, at);
    let mut items: Vec<_> = FIELDS
        .into_iter()
        .map(|(label, insert, snippet)| {
            let mut item = CompletionItem::new(label, CompletionKind::Property);
            item.insert_text = insert.into();
            item.snippet = snippet;
            item.replace = replace;
            item
        })
        .collect();
    let mut environment = CompletionItem::new("env", CompletionKind::Property);
    environment.detail = Some("Task environment (TOML 1.0 compatible)".into());
    environment.insert_text = "env.${1:NAME} = \"$0\"".into();
    environment.snippet = true;
    environment.replace = replace;
    environment.documentation = Some(format!(
        "Adds an environment variable to `[{task_section}]` using a dotted key. This avoids false syntax errors from TOML language servers that do not yet accept TOML 1.1 multiline inline tables."
    ));
    items.push(environment);
    items
}

fn is_direct_task_section(section: &str) -> bool {
    toml_edit::Key::parse(section)
        .is_ok_and(|keys| keys.len() == 2 && keys.first().is_some_and(|key| key.get() == "tasks"))
}

fn code_actions(doc: &ProviderDocument, selection: Range) -> Vec<CodeAction> {
    if !is_mise_config_provider_document(doc) {
        return Vec::new();
    }

    multiline_task_env_edits(&doc.text)
        .into_iter()
        .filter(|edit| ranges_touch(edit.range, selection))
        .map(|edit| CodeAction {
            title: "Mise: Convert multiline env to TOML 1.0-compatible keys".into(),
            kind: Some("quickfix".into()),
            preferred: true,
            edits: vec![(doc.path.clone(), vec![edit])],
            data: None,
        })
        .collect()
}

fn multiline_task_env_edits(source: &str) -> Vec<TextEdit> {
    let mut edits = Vec::new();
    let mut section = None;
    let mut offset = 0usize;

    for raw_line in source.split_inclusive('\n') {
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let leading = line.len() - line.trim_start().len();
        let trimmed = line.trim_start();
        if trimmed.starts_with('[')
            && !trimmed.starts_with("[[")
            && let Some(close) = trimmed.find(']')
        {
            section = Some(trimmed[1..close].trim().to_owned());
            offset += raw_line.len();
            continue;
        }
        if !section.as_deref().is_some_and(is_direct_task_section) {
            offset += raw_line.len();
            continue;
        }
        let Some(equal) = assignment_equal(trimmed) else {
            offset += raw_line.len();
            continue;
        };
        if unquoted_key(trimmed[..equal].trim()) != "env" {
            offset += raw_line.len();
            continue;
        }
        let value_absolute = offset + leading + equal + 1;
        let (value_start, value_end) = assignment_value_range(source, value_absolute);
        let value_text = &source[value_start..value_end];
        if !value_text.starts_with('{') || !value_text.contains('\n') || value_text.contains('#') {
            offset += raw_line.len();
            continue;
        }
        let Ok(value) = toml_edit::Value::from_str(value_text) else {
            offset += raw_line.len();
            continue;
        };
        let Some(table) = value.as_inline_table() else {
            offset += raw_line.len();
            continue;
        };
        let mut assignments = Vec::new();
        for (key, value) in table.iter() {
            let key = table
                .key(key)
                .map(ToString::to_string)
                .unwrap_or_else(|| toml_edit::Key::new(key).to_string());
            let mut value = value.clone();
            value.decor_mut().clear();
            assignments.push(format!("env.{key} = {value}"));
        }
        if assignments.is_empty() {
            offset += raw_line.len();
            continue;
        }
        let key_start = offset + leading;
        edits.push(TextEdit {
            range: byte_range(source, key_start, value_end),
            text: assignments.join("\n"),
        });
        offset += raw_line.len();
    }
    edits
}

fn ranges_touch(left: Range, right: Range) -> bool {
    left.start <= right.end && right.start <= left.end
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

fn is_mise_config_document(path: &str) -> bool {
    is_config_path(path)
}

fn is_mise_config_provider_document(doc: &ProviderDocument) -> bool {
    doc.language == "toml" && is_mise_config_document(&doc.path)
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
    fn task_environment_completion_uses_toml_1_0_compatible_dotted_keys() {
        let doc = ProviderDocument {
            path: "/project/mise.toml".into(),
            language: "toml".into(),
            version: 1,
            text: "[tasks.\"build:web\"]\nen".into(),
        };
        let items = completion(
            &doc,
            Position {
                line: 1,
                character: 2,
            },
            None,
        );
        let environment = items.iter().find(|item| item.label == "env").unwrap();
        assert_eq!(environment.insert_text, "env.${1:NAME} = \"$0\"");
        assert!(
            environment
                .documentation
                .as_deref()
                .unwrap()
                .contains("[tasks.\"build:web\"]")
        );

        let nested = ProviderDocument {
            text: "[tasks.dev.env]\nVI".into(),
            ..doc
        };
        assert!(
            completion(
                &nested,
                Position {
                    line: 1,
                    character: 2,
                },
                None,
            )
            .iter()
            .all(|item| item.label != "run")
        );
    }

    #[test]
    fn accepts_mise_toml_1_1_task_env_and_offers_a_taplo_compatible_fix() {
        let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
        host.set_project_root(Some("/project"));
        let source = r#"[tasks.dev]
run = 'npm run dev'
env = {
  VITE_APP_USER_SITE_URL = 'http://localhost:5173',
  VITE_APP_TURNSTILE_ENABLED = 'false',
  VITE_APP_TURNSTILE_SITE_KEY = '',
}
"#;
        let doc = ProviderDocument {
            path: "/project/mise.toml".into(),
            language: "toml".into(),
            version: 1,
            text: source.into(),
        };
        assert!(
            diagnostics(&doc)
                .iter()
                .all(|diagnostic| diagnostic.code.as_deref() != Some("mise.invalid-toml"))
        );

        let actions = code_actions(&doc, Range::new(Position::new(4, 42), Position::new(4, 43)));
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].kind.as_deref(), Some("quickfix"));
        let edit = &actions[0].edits[0].1[0];
        assert_eq!(
            edit.text,
            concat!(
                "env.VITE_APP_USER_SITE_URL = 'http://localhost:5173'\n",
                "env.VITE_APP_TURNSTILE_ENABLED = 'false'\n",
                "env.VITE_APP_TURNSTILE_SITE_KEY = ''"
            )
        );
        let start = text::offset(source, edit.range.start).unwrap();
        let end = text::offset(source, edit.range.end).unwrap();
        let fixed = format!("{}{}{}", &source[..start], edit.text, &source[end..]);
        let parsed = toml_edit::DocumentMut::from_str(&fixed).unwrap();
        assert_eq!(
            parsed["tasks"]["dev"]["env"]["VITE_APP_USER_SITE_URL"].as_str(),
            Some("http://localhost:5173")
        );
        assert_eq!(
            parsed["tasks"]["dev"]["env"]["VITE_APP_TURNSTILE_ENABLED"].as_str(),
            Some("false")
        );
    }

    #[test]
    fn shell_injection_virtual_documents_are_not_reparsed_as_mise_toml() {
        let injected = ProviderDocument {
            path: "/project/mise.toml".into(),
            language: "shellscript".into(),
            version: 1,
            text: format!(
                "{}                 ./gradlew --console=plain bootRun\n",
                "\n".repeat(15)
            ),
        };

        assert!(diagnostics(&injected).is_empty());
        assert!(semantic_tokens(&injected).is_empty());
        assert!(
            completion(&injected, Position::new(15, 25), None).is_empty(),
            "the masked Shell document must not receive TOML completion"
        );
        assert!(hover(&injected, Position::new(15, 25)).is_none());
        assert!(code_actions(&injected, Range::default()).is_empty());
        assert!(document_symbols(&injected).is_empty());
        assert!(gutter_actions(&injected).is_empty());
        assert!(injections(&injected).is_empty());
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
    fn structure_view_groups_tools_tasks_and_environment() {
        let doc = FeatureDocument {
            path: "/project/mise.toml".into(),
            language: "toml".into(),
            version: 1,
            text: format!("{CONFIG}\n[env]\nNODE_ENV = \"test\"\n"),
        };

        let symbols = document_symbols(&doc);

        let roots: Vec<_> = symbols
            .iter()
            .filter(|symbol| symbol.parent.is_none())
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert_eq!(roots, ["Tools", "Tasks", "Environment"]);
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == "build:web" && symbol.kind == "function")
        );
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == "NODE_ENV" && symbol.kind == "variable")
        );
    }

    #[test]
    fn diagnostics_report_invalid_unresolved_and_cyclic_tasks() {
        let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
        host.set_project_root(Some("/project"));
        let unresolved = ProviderDocument {
            path: "/project/mise.toml".into(),
            language: "toml".into(),
            version: 1,
            text: r#"[tasks.a]
depends = ["missing"]

[tasks.b]
depends = ["c"]

[tasks.c]
depends = ["b"]
"#
            .into(),
        };

        let problems = diagnostics(&unresolved);
        assert!(
            problems
                .iter()
                .any(|problem| problem.code.as_deref() == Some("mise.unresolved-task"))
        );
        assert_eq!(
            problems
                .iter()
                .filter(|problem| problem.code.as_deref() == Some("mise.dependency-cycle"))
                .count(),
            2
        );

        let invalid = ProviderDocument {
            text: "[tasks.build\nrun = \"echo hi\"".into(),
            ..unresolved
        };
        assert!(
            diagnostics(&invalid)
                .iter()
                .any(|problem| problem.code.as_deref() == Some("mise.invalid-toml"))
        );
    }

    #[test]
    fn semantic_tokens_mark_mise_definitions_and_references() {
        let doc = provider_doc();
        let tokens = semantic_tokens(&doc);

        assert!(tokens.iter().any(|token| {
            token.kind == TokenKind::Function
                && token.modifiers.contains(&TokenModifier::Definition)
        }));
        assert!(tokens.iter().any(|token| token.kind == TokenKind::Property));
    }

    #[test]
    fn file_tasks_support_metadata_completion_navigation_and_run_gutter() {
        let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
        host.set_project_root(Some("/project"));
        state::save(&Snapshot {
            project: "/project".into(),
            context: "/project".into(),
            tasks: vec![
                MiseTask {
                    name: "lint".into(),
                    source: "/project/.mise/tasks/lint".into(),
                    ..Default::default()
                },
                MiseTask {
                    name: "group:build".into(),
                    source: "/project/.mise/tasks/group/build.sh".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        })
        .unwrap();
        let doc = ProviderDocument {
            path: "/project/.mise/tasks/group/build.sh".into(),
            language: "shellscript".into(),
            version: 1,
            text: "#!/bin/sh\n#MISE depends=[\"lint\"]\necho build\n".into(),
        };

        let metadata = completion(
            &ProviderDocument {
                text: "#!/bin/sh\n#MISE dep".into(),
                ..doc.clone()
            },
            Position {
                line: 1,
                character: 9,
            },
            None,
        );
        assert!(metadata.iter().any(|item| item.label == "depends"));

        let definitions = definition(
            &doc,
            Position {
                line: 1,
                character: 17,
            },
        );
        assert_eq!(definitions[0].path, "/project/.mise/tasks/lint");

        let feature_doc = FeatureDocument {
            path: doc.path.clone(),
            language: doc.language.clone(),
            version: doc.version,
            text: doc.text.clone(),
        };
        let actions = gutter_actions(&feature_doc);
        assert_eq!(actions[0].argument.as_deref(), Some("group:build"));
        assert_eq!(actions[0].line, 2);
        assert!(
            semantic_tokens(&doc)
                .iter()
                .any(|token| token.kind == TokenKind::Property)
        );
    }

    #[test]
    fn file_task_diagnostics_reject_unknown_metadata_and_self_dependencies() {
        let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
        host.set_project_root(Some("/project"));
        let doc = ProviderDocument {
            path: "/project/mise-tasks/build".into(),
            language: "shellscript".into(),
            version: 1,
            text: "#MISE mystery=true\n#MISE depends=[\"build\"]\necho build\n".into(),
        };

        let problems = diagnostics(&doc);

        assert!(
            problems
                .iter()
                .any(|problem| problem.code.as_deref() == Some("mise.unknown-file-task-metadata"))
        );
        assert!(
            problems
                .iter()
                .any(|problem| problem.code.as_deref() == Some("mise.self-dependency"))
        );
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
