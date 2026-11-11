use crate::model::{CachedSnapshot, Snapshot, stable_hash};
use openidea_plugin::{Error, host};

const SNAPSHOT_PREFIX: &str = "project-snapshot.";

pub fn cache_key(project_root: &str) -> String {
    format!("{SNAPSHOT_PREFIX}{:016x}", stable_hash(project_root))
}

pub fn load(project_root: &str) -> Result<Option<Snapshot>, Error> {
    let Some(value) = host::storage::get(&cache_key(project_root))? else {
        return Ok(None);
    };
    let cached: CachedSnapshot = serde_json::from_str(&value)
        .map_err(|error| Error::Invalid(format!("invalid cached mise snapshot: {error}")))?;
    if cached.project != project_root {
        return Ok(None);
    }
    Ok(Some(cached.into()))
}

pub fn save(snapshot: &Snapshot) -> Result<(), Error> {
    let cached = CachedSnapshot::from(snapshot);
    let text = serde_json::to_string(&cached)
        .map_err(|error| Error::Failed(format!("could not serialize mise snapshot: {error}")))?;
    host::storage::set(&cache_key(&snapshot.project), &text)
}

pub fn delete(project_root: &str) {
    let _ = host::storage::delete(&cache_key(project_root));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MiseTask;
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
        let loaded = load("/project").unwrap().unwrap();
        assert_eq!(loaded.tasks[0].name, "build");
        assert!(loaded.environment.is_empty());
    }
}
