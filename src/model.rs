use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const MAX_TASKS: usize = 4_096;
pub const MAX_TOOLS: usize = 4_096;
pub const MAX_ENVIRONMENT_VALUES: usize = 8_192;
pub const MAX_CONFIG_FILES: usize = 512;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MiseTask {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub depends_post: Vec<String>,
    #[serde(default)]
    pub wait_for: Vec<String>,
    #[serde(default)]
    pub dir: String,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MiseTool {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub requested_version: String,
    #[serde(default)]
    pub source: String,
    pub installed: bool,
    pub active: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MiseConfigFile {
    pub path: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub override_file: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvironmentValue {
    pub name: String,
    pub value: String,
    pub sensitive: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub project: String,
    pub context: String,
    pub tasks: Vec<MiseTask>,
    pub tools: Vec<MiseTool>,
    pub configs: Vec<MiseConfigFile>,
    pub environment: Vec<EnvironmentValue>,
    pub errors: Vec<String>,
    pub trust_required: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedSnapshot {
    pub project: String,
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub tasks: Vec<MiseTask>,
    #[serde(default)]
    pub tools: Vec<MiseTool>,
    #[serde(default)]
    pub configs: Vec<MiseConfigFile>,
}

impl From<&Snapshot> for CachedSnapshot {
    fn from(value: &Snapshot) -> Self {
        Self {
            project: value.project.clone(),
            context: value.context.clone(),
            tasks: value.tasks.clone(),
            tools: value.tools.clone(),
            configs: value.configs.clone(),
        }
    }
}

impl From<CachedSnapshot> for Snapshot {
    fn from(value: CachedSnapshot) -> Self {
        Self {
            project: value.project,
            context: value.context,
            tasks: value.tasks,
            tools: value.tools,
            configs: value.configs,
            environment: Vec::new(),
            errors: Vec::new(),
            trust_required: false,
        }
    }
}

impl Snapshot {
    pub fn view_json(&self, redact_environment: bool, auto_refresh_seconds: u64) -> Value {
        let graph = crate::graph::build(&self.tasks);
        let environment: Vec<Value> = self
            .environment
            .iter()
            .map(|entry| {
                let redacted = redact_environment && entry.sensitive;
                serde_json::json!({
                    "name": entry.name,
                    "value": if redacted { "••••••••" } else { &entry.value },
                    "sensitive": entry.sensitive,
                    "redacted": redacted,
                })
            })
            .collect();
        serde_json::json!({
            "op": "snapshot",
            "project": self.project,
            "context": self.context,
            "tasks": self.tasks,
            "graph": graph,
            "tools": self.tools,
            "configs": self.configs,
            "environment": environment,
            "errors": self.errors,
            "trustRequired": self.trust_required,
            "autoRefreshSeconds": auto_refresh_seconds,
        })
    }
}

pub fn parse_tasks(text: &str) -> Result<Vec<MiseTask>, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|error| format!("invalid task JSON: {error}"))?;
    let rows = value
        .as_array()
        .ok_or_else(|| "task JSON must be an array".to_owned())?;
    if rows.len() > MAX_TASKS {
        return Err(format!(
            "task JSON contains {} entries; the safety limit is {MAX_TASKS}",
            rows.len()
        ));
    }
    let mut tasks = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(name) = string_field(row, "name") else {
            continue;
        };
        tasks.push(MiseTask {
            name,
            aliases: string_list(row.get("aliases")),
            description: string_field(row, "description").unwrap_or_default(),
            source: string_field(row, "source").unwrap_or_default(),
            depends: task_refs(row.get("depends")),
            depends_post: task_refs(row.get("depends_post")),
            wait_for: task_refs(row.get("wait_for")),
            dir: string_field(row, "dir").unwrap_or_default(),
            hidden: row.get("hide").and_then(Value::as_bool).unwrap_or(false),
        });
    }
    tasks.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(tasks)
}

pub fn parse_tools(text: &str) -> Result<Vec<MiseTool>, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|error| format!("invalid tool JSON: {error}"))?;
    let tools = value
        .as_object()
        .ok_or_else(|| "tool JSON must be an object".to_owned())?;
    let mut parsed = Vec::new();
    for (name, entries) in tools {
        let Some(entries) = entries.as_array() else {
            continue;
        };
        for entry in entries {
            if parsed.len() >= MAX_TOOLS {
                return Err(format!(
                    "tool JSON contains more than {MAX_TOOLS} entries; narrow the Mise context"
                ));
            }
            let Some(version) = string_field(entry, "version") else {
                continue;
            };
            let source = entry
                .get("source")
                .and_then(Value::as_object)
                .and_then(|source| source.get("path"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            parsed.push(MiseTool {
                name: name.clone(),
                version,
                requested_version: string_field(entry, "requested_version").unwrap_or_default(),
                source,
                installed: entry
                    .get("installed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                active: entry
                    .get("active")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            });
        }
    }
    parsed.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| right.active.cmp(&left.active))
            .then_with(|| left.version.cmp(&right.version))
    });
    Ok(parsed)
}

pub fn parse_environment(text: &str) -> Result<Vec<EnvironmentValue>, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|error| format!("invalid environment JSON: {error}"))?;
    let values = value
        .as_object()
        .ok_or_else(|| "environment JSON must be an object".to_owned())?;
    if values.len() > MAX_ENVIRONMENT_VALUES {
        return Err(format!(
            "environment JSON contains {} entries; the safety limit is {MAX_ENVIRONMENT_VALUES}",
            values.len()
        ));
    }
    let mut parsed = BTreeMap::new();
    for (name, value) in values {
        let text = match value {
            Value::String(value) => value.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        parsed.insert(
            name.clone(),
            EnvironmentValue {
                name: name.clone(),
                value: text,
                sensitive: is_sensitive_name(name),
            },
        );
    }
    Ok(parsed.into_values().collect())
}

pub fn parse_config_files(text: &str, project_root: &str) -> Result<Vec<MiseConfigFile>, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|error| format!("invalid config JSON: {error}"))?;
    let rows = value
        .as_array()
        .ok_or_else(|| "config JSON must be an array".to_owned())?;
    if rows.len() > MAX_CONFIG_FILES {
        return Err(format!(
            "config JSON contains {} entries; the safety limit is {MAX_CONFIG_FILES}",
            rows.len()
        ));
    }

    let mut configs = Vec::with_capacity(rows.len());
    for row in rows {
        let (path, tools, tier, override_file) = match row {
            Value::String(path) => (path.clone(), Vec::new(), String::new(), false),
            Value::Object(object) => {
                let Some(path) = object.get("path").and_then(Value::as_str) else {
                    continue;
                };
                let tier = object
                    .get("tier")
                    .or_else(|| object.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let override_file = object
                    .get("override")
                    .and_then(Value::as_bool)
                    .unwrap_or_else(|| path.contains(".override."));
                (
                    path.to_owned(),
                    string_list(object.get("tools")),
                    tier,
                    override_file,
                )
            }
            _ => continue,
        };
        configs.push(MiseConfigFile {
            scope: config_scope(&path, project_root),
            path,
            tools,
            tier,
            override_file,
        });
    }
    Ok(configs)
}

pub fn is_sensitive_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let parts: Vec<&str> = upper
        .split(|character: char| !character.is_ascii_alphanumeric())
        .collect();
    parts.iter().any(|part| {
        matches!(
            *part,
            "SECRET"
                | "TOKEN"
                | "PASSWORD"
                | "PASS"
                | "KEY"
                | "CREDENTIAL"
                | "CREDENTIALS"
                | "AUTH"
                | "COOKIE"
                | "PRIVATE"
                | "BEARER"
        )
    })
}

pub fn task_configuration_id(name: &str) -> String {
    format!("mise-task-{:016x}", stable_hash(name))
}

pub fn stable_hash(value: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn string_field(value: &Value, name: &str) -> Option<String> {
    value.get(name).and_then(|field| match field {
        Value::String(value) => Some(value.clone()),
        Value::Object(value) => value
            .get("path")
            .or_else(|| value.get("source"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    })
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn task_refs(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| match item {
            Value::String(value) => Some(value.clone()),
            Value::Object(value) => value
                .get("task")
                .or_else(|| value.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            _ => None,
        })
        .collect()
}

fn config_scope(path: &str, project_root: &str) -> String {
    let path = normalize_path(path);
    let project_root = normalize_path(project_root)
        .trim_end_matches('/')
        .to_owned();
    let comparable_config = comparable_path(&path);
    let comparable_root = comparable_path(&project_root);
    if comparable_config == comparable_root
        || comparable_config.starts_with(&format!("{comparable_root}/"))
    {
        let relative = comparable_config
            .strip_prefix(&comparable_root)
            .unwrap_or_default()
            .trim_start_matches('/');
        return if relative.contains('/') {
            "nested"
        } else {
            "project"
        }
        .into();
    }
    if path == "/etc/mise" || path.starts_with("/etc/mise/") {
        return "system".into();
    }
    if path.contains("/.config/mise/") || path.ends_with("/.config/mise") {
        return "global".into();
    }
    "inherited".into()
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn comparable_path(path: &str) -> String {
    let normalized = normalize_path(path);
    if normalized.as_bytes().get(1) == Some(&b':') || normalized.starts_with("//") {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sanitized_current_and_forward_compatible_mise_fixtures() {
        let tasks = parse_tasks(include_str!("../tests/fixtures/mise/tasks-current.json")).unwrap();
        assert_eq!(
            tasks
                .iter()
                .map(|task| task.name.as_str())
                .collect::<Vec<_>>(),
            ["build", "lint"]
        );
        assert_eq!(tasks[0].depends_post, ["notify"]);

        let future = parse_tasks(include_str!(
            "../tests/fixtures/mise/tasks-forward-compatible.json"
        ))
        .unwrap();
        assert_eq!(future[0].source, "/workspace/apps/web/mise.toml");
        assert_eq!(future[0].depends, ["build"]);
        assert_eq!(future[0].depends_post, ["announce"]);

        let tools = parse_tools(include_str!("../tests/fixtures/mise/tools-current.json")).unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "node");
        assert_eq!(tools[1].source, "/home/user/.config/mise/config.toml");

        let configs = parse_config_files(
            include_str!("../tests/fixtures/mise/config-current.json"),
            "/workspace",
        )
        .unwrap();
        assert_eq!(configs[0].scope, "nested");
        assert!(configs[0].override_file);
        assert_eq!(configs[2].scope, "global");

        let environment = parse_environment(include_str!(
            "../tests/fixtures/mise/environment-current.json"
        ))
        .unwrap();
        assert_eq!(
            environment
                .iter()
                .find(|entry| entry.name == "HTTP_PORT")
                .unwrap()
                .value,
            "5173"
        );
        assert_eq!(
            environment
                .iter()
                .find(|entry| entry.name == "OPTIONAL_VALUE")
                .unwrap()
                .value,
            ""
        );
    }

    #[test]
    fn parses_current_mise_task_shape() {
        let tasks = parse_tasks(
            r#"[
              {
                "name":"build",
                "aliases":["b"],
                "description":"Build",
                "source":"/p/mise.toml",
                "depends":["lint",{"task":"prepare"}],
                "depends_post":[],
                "wait_for":["service"],
                "dir":"/p",
                "hide":false
              }
            ]"#,
        )
        .unwrap();
        assert_eq!(tasks[0].name, "build");
        assert_eq!(tasks[0].depends, ["lint", "prepare"]);
        assert_eq!(tasks[0].wait_for, ["service"]);
    }

    #[test]
    fn parses_config_hierarchy_and_forward_compatible_metadata() {
        let configs = parse_config_files(
            r#"[
              {"path":"/project/backend/mise.toml","tools":["node"],"tier":"project"},
              {"path":"/project/mise.toml","tools":["rust"]},
              {"path":"/home/me/.config/mise/config.override.toml","override":true},
              "/etc/mise/config.toml"
            ]"#,
            "/project",
        )
        .unwrap();

        assert_eq!(configs[0].scope, "nested");
        assert_eq!(configs[1].scope, "project");
        assert_eq!(configs[2].scope, "global");
        assert!(configs[2].override_file);
        assert_eq!(configs[3].scope, "system");
        assert_eq!(configs[0].tier, "project");
    }

    #[test]
    fn accepts_object_task_sources_from_newer_mise_versions() {
        let tasks =
            parse_tasks(r#"[{"name":"build","source":{"path":"/project/mise.toml"}}]"#).unwrap();
        assert_eq!(tasks[0].source, "/project/mise.toml");
    }

    #[test]
    fn rejects_unbounded_cli_results() {
        let tasks = format!(
            "[{}]",
            (0..=MAX_TASKS)
                .map(|index| format!(r#"{{"name":"task-{index}"}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(parse_tasks(&tasks).unwrap_err().contains("safety limit"));
    }

    #[test]
    fn parses_and_orders_tool_versions() {
        let tools = parse_tools(
            r#"{
              "node":[
                {"version":"20.0.0","installed":true,"active":false},
                {"version":"22.0.0","requested_version":"22","installed":true,"active":true,
                 "source":{"type":"mise.toml","path":"/p/mise.toml"}}
              ]
            }"#,
        )
        .unwrap();
        assert_eq!(tools[0].version, "22.0.0");
        assert_eq!(tools[0].requested_version, "22");
        assert_eq!(tools[0].source, "/p/mise.toml");
    }

    #[test]
    fn redacts_secret_like_names_without_hiding_path() {
        assert!(is_sensitive_name("API_TOKEN"));
        assert!(is_sensitive_name("AWS_ACCESS_KEY_ID"));
        assert!(is_sensitive_name("database-password"));
        assert!(!is_sensitive_name("PATH"));
        assert!(!is_sensitive_name("MONKEY"));
    }

    #[test]
    fn task_ids_are_stable_and_manifest_safe() {
        let id = task_configuration_id("build:frontend");
        assert_eq!(id, task_configuration_id("build:frontend"));
        assert!(id.chars().all(|character| character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || character == '-'));
    }
}
