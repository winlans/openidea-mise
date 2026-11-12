use crate::model::{CachedSnapshot, MAX_CONFIG_FILES, MAX_TASKS, MAX_TOOLS, Snapshot, stable_hash};
use openidea_plugin::{Error, host};

const SNAPSHOT_PREFIX: &str = "project-snapshot.";
const MAX_CACHE_BYTES: usize = 2 * 1024 * 1024;

pub fn cache_key(project_root: &str) -> String {
    format!("{SNAPSHOT_PREFIX}{:016x}", stable_hash(project_root))
}

pub fn load(project_root: &str, context: &str) -> Result<Option<Snapshot>, Error> {
    let Some(value) = host::storage::get(&cache_key(project_root))? else {
        return Ok(None);
    };
    let cached: CachedSnapshot = serde_json::from_str(&value)
        .map_err(|error| Error::Invalid(format!("invalid cached mise snapshot: {error}")))?;
    if cached.project != project_root {
        return Ok(None);
    }
    if cached.context.is_empty() {
        if normalize_path(project_root) != normalize_path(context) {
            return Ok(None);
        }
    } else if normalize_path(&cached.context) != normalize_path(context) {
        return Ok(None);
    }
    Ok(Some(cached.into()))
}

pub fn save(snapshot: &Snapshot) -> Result<(), Error> {
    let mut cached = CachedSnapshot::from(snapshot);
    cached.tasks.truncate(MAX_TASKS.min(2_048));
    cached.tools.truncate(MAX_TOOLS.min(2_048));
    cached.configs.truncate(MAX_CONFIG_FILES);
    let text = serde_json::to_string(&cached)
        .map_err(|error| Error::Failed(format!("could not serialize mise snapshot: {error}")))?;
    if text.len() > MAX_CACHE_BYTES {
        return Err(Error::Invalid(format!(
            "mise cache is {} bytes; the safety limit is {MAX_CACHE_BYTES}",
            text.len()
        )));
    }
    host::storage::set(&cache_key(&snapshot.project), &text)
}

pub fn delete(project_root: &str) {
    let _ = host::storage::delete(&cache_key(project_root));
}

fn normalize_path(path: &str) -> String {
    let normalized = path.replace('\\', "/").trim_end_matches('/').to_owned();
    if normalized.as_bytes().get(1) == Some(&b':') || normalized.starts_with("//") {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MiseConfigFile, MiseTask, MiseTool};
    use openidea_plugin::mock::MockHost;

    #[test]
    fn environment_is_never_persisted() {
        let _host = MockHost::install().with_capabilities(&["storage"]);
        let snapshot = Snapshot {
            project: "/project".into(),
            tasks: vec![MiseTask {
                name: "build".into(),
                ..Default::default()
            }],
            environment: vec![crate::model::EnvironmentValue {
                name: "API_TOKEN".into(),
                value: "secret".into(),
                sensitive: true,
            }],
            ..Default::default()
        };
        save(&snapshot).unwrap();
        let loaded = load("/project", "/project").unwrap().unwrap();
        assert_eq!(loaded.tasks[0].name, "build");
        assert!(loaded.environment.is_empty());
    }

    #[test]
    fn a_cache_from_another_directory_context_is_not_reused() {
        let _host = MockHost::install().with_capabilities(&["storage"]);
        save(&Snapshot {
            project: "/project".into(),
            context: "/project/backend".into(),
            ..Default::default()
        })
        .unwrap();

        assert!(load("/project", "/project").unwrap().is_none());
        assert!(load("/project", "/project/backend").unwrap().is_some());
    }

    #[test]
    fn a_legacy_cache_without_context_is_only_used_at_the_project_root() {
        let _host = MockHost::install().with_capabilities(&["storage"]);
        host::storage::set(
            &cache_key("/project"),
            r#"{"project":"/project","tasks":[],"tools":[]}"#,
        )
        .unwrap();

        assert!(load("/project", "/project/").unwrap().is_some());
        assert!(load("/project", "/project/backend").unwrap().is_none());
    }

    #[test]
    fn persistent_cache_truncates_unbounded_collections() {
        let _host = MockHost::install().with_capabilities(&["storage"]);
        save(&Snapshot {
            project: "/project".into(),
            context: "/project".into(),
            tasks: (0..2_100)
                .map(|index| MiseTask {
                    name: format!("task-{index}"),
                    ..Default::default()
                })
                .collect(),
            tools: (0..2_100)
                .map(|index| MiseTool {
                    name: format!("tool-{index}"),
                    ..Default::default()
                })
                .collect(),
            configs: (0..600)
                .map(|index| MiseConfigFile {
                    path: format!("/project/config-{index}.toml"),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })
        .unwrap();

        let loaded = load("/project", "/project").unwrap().unwrap();
        assert_eq!(loaded.tasks.len(), 2_048);
        assert_eq!(loaded.tools.len(), 2_048);
        assert_eq!(loaded.configs.len(), MAX_CONFIG_FILES);
    }

    #[test]
    fn persistent_cache_rejects_an_oversized_serialized_value() {
        let _host = MockHost::install().with_capabilities(&["storage"]);
        let error = save(&Snapshot {
            project: "/project".into(),
            context: "/project".into(),
            tasks: vec![MiseTask {
                name: "huge".into(),
                description: "x".repeat(MAX_CACHE_BYTES),
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap_err();

        assert!(error.to_string().contains("safety limit"));
        assert!(load("/project", "/project").unwrap().is_none());
    }
}
