use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvironmentValue {
    pub name: String,
    pub value: String,
    pub sensitive: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub project: String,
    pub tasks: Vec<MiseTask>,
    pub tools: Vec<MiseTool>,
    pub environment: Vec<EnvironmentValue>,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedSnapshot {
    pub project: String,
    #[serde(default)]
    pub tasks: Vec<MiseTask>,
    #[serde(default)]
    pub tools: Vec<MiseTool>,
}

impl From<&Snapshot> for CachedSnapshot {
    fn from(value: &Snapshot) -> Self {
        Self {
            project: value.project.clone(),
            tasks: value.tasks.clone(),
            tools: value.tools.clone(),
        }
    }
}

impl From<CachedSnapshot> for Snapshot {
    fn from(value: CachedSnapshot) -> Self {
        Self {
            project: value.project,
            tasks: value.tasks,
            tools: value.tools,
            environment: Vec::new(),
            errors: Vec::new(),
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
            "tasks": self.tasks,
            "graph": graph,
            "tools": self.tools,
            "environment": environment,
            "errors": self.errors,
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
    value.get(name).and_then(Value::as_str).map(str::to_owned)
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

#[cfg(test)]
mod tests {
    use super::*;

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
