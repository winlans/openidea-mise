mod graph;
mod mise;
mod model;
mod state;
mod toml_support;

use crate::model::{MiseTask, Snapshot, task_configuration_id};
use openidea_plugin::views::{Body, Button, View, ViewEvent, Viewport, Views};
use openidea_plugin::{
    Error, Event, Invocation, Plugin, PluginRunConfiguration, PluginRunTarget, RunConfigurations,
    host, register_plugin,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const VIEW: &str = "mise";
const PAGE: &str = include_str!("page.html");
const CONFIG_FILES: [&str; 4] = [
    "mise.toml",
    ".mise.toml",
    "mise.local.toml",
    ".mise.local.toml",
];

#[derive(Default)]
pub struct OpenideaMise {
    snapshots: BTreeMap<String, Snapshot>,
}

impl OpenideaMise {
    fn root(ctx: &Invocation) -> Result<(openidea_plugin::Project, String), Error> {
        let project = ctx
            .project()
            .ok_or_else(|| Error::Invalid("open a project first".into()))?;
        let root = project.root()?;
        Ok((project, root))
    }

    fn snapshot(&mut self, root: &str) -> Snapshot {
        if let Some(snapshot) = self.snapshots.get(root) {
            return snapshot.clone();
        }
        let snapshot = state::load(root)
            .ok()
            .flatten()
            .unwrap_or_else(|| Snapshot {
                project: root.to_owned(),
                errors: vec!["Refresh to load Mise data for this project.".into()],
                ..Default::default()
            });
        self.snapshots.insert(root.to_owned(), snapshot.clone());
        snapshot
    }

    fn refresh(&mut self, ctx: &Invocation) -> Result<Snapshot, Error> {
        let (project, root) = Self::root(ctx)?;
        let mut snapshot = self.snapshot(&root);
        snapshot.project = root.clone();
        snapshot.errors.clear();

        match mise::tasks(&project) {
            Ok(tasks) => snapshot.tasks = tasks,
            Err(error) => snapshot.errors.push(format!("Tasks: {error}")),
        }
        match mise::tools(&project) {
            Ok(tools) => snapshot.tools = tools,
            Err(error) => snapshot.errors.push(format!("Tools: {error}")),
        }
        match mise::environment(&project) {
            Ok(environment) => snapshot.environment = environment,
            Err(error) => {
                snapshot.environment.clear();
                snapshot.errors.push(format!("Environment: {error}"));
            }
        }

        if let Err(error) = state::save(&snapshot) {
            snapshot.errors.push(format!("Cache: {error}"));
        }
        if let Err(error) = configure_run_target(&project)
            && !matches!(error, Error::Denied(_))
        {
            snapshot.errors.push(format!("Run target: {error}"));
        }
        self.snapshots.insert(root, snapshot.clone());
        Ok(snapshot)
    }

    fn post_snapshot(&mut self, ctx: &Invocation, refresh: bool) -> Result<(), Error> {
        let (_, root) = Self::root(ctx)?;
        let snapshot = if refresh {
            self.refresh(ctx)?
        } else {
            self.snapshot(&root)
        };
        host::post_view_message(VIEW, &snapshot_message(&snapshot).to_string())
    }

    fn run_task(
        &mut self,
        ctx: &Invocation,
        task_name: Option<&str>,
    ) -> Result<Option<String>, Error> {
        let (_, root) = Self::root(ctx)?;
        let snapshot = self.snapshot(&root);
        let task = match task_name {
            Some(name) => snapshot
                .tasks
                .iter()
                .find(|task| task.name == name)
                .ok_or_else(|| Error::NotFound(format!("mise task {name}")))?,
            None => {
                let items: Vec<_> = snapshot
                    .tasks
                    .iter()
                    .map(|task| {
                        openidea_plugin::PickItem::new(&task.name, &task.name)
                            .description(&task.description)
                    })
                    .collect();
                let Some(chosen) = host::pick("Run Mise task", &items)? else {
                    return Err(Error::Cancelled);
                };
                snapshot
                    .tasks
                    .iter()
                    .find(|task| task.name == chosen)
                    .ok_or_else(|| Error::NotFound(format!("mise task {chosen}")))?
            }
        };
        host::start_run(&task_configuration_id(&task.name), &[], false).map(Some)
    }

    fn invalidate(&mut self, ctx: &Invocation) {
        let Ok((_, root)) = Self::root(ctx) else {
            return;
        };
        self.snapshots.remove(&root);
        state::delete(&root);
        host::refresh_view(VIEW);
    }
}

impl Plugin for OpenideaMise {
    fn activate(&mut self, _ctx: &openidea_plugin::Context) -> Result<(), Error> {
        let globs = [
            "**/mise.toml",
            "**/.mise.toml",
            "**/mise.local.toml",
            "**/.mise.local.toml",
        ];
        host::subscribe_paths("file-saved", &globs)?;
        host::subscribe_paths("files-changed", &globs)?;
        Ok(())
    }

    fn run_command(
        &mut self,
        id: &str,
        ctx: &Invocation,
        arg: Option<&str>,
    ) -> Result<Option<String>, Error> {
        match id {
            "show" => {
                host::reveal_view(VIEW)?;
                Ok(None)
            }
            "refresh" => {
                self.post_snapshot(ctx, true)?;
                host::reveal_view(VIEW)?;
                Ok(None)
            }
            "run-task" => self.run_task(ctx, arg),
            _ => Err(Error::NotFound(format!("command {id}"))),
        }
    }

    fn handle_events(&mut self, events: &[Event], ctx: &Invocation) {
        for event in events {
            match event {
                Event::FileSaved(path) if is_config_path(path) => self.invalidate(ctx),
                Event::FilesChanged(changes)
                    if changes
                        .created
                        .iter()
                        .chain(&changes.modified)
                        .chain(&changes.removed)
                        .any(|path| is_config_path(path)) =>
                {
                    self.invalidate(ctx);
                }
                Event::Other { kind, .. } if kind == "settings-changed" => {
                    if let Ok((project, _)) = Self::root(ctx) {
                        let _ = configure_run_target(&project);
                    }
                    let _ = self.post_snapshot(ctx, false);
                }
                _ => {}
            }
        }
    }
}

impl Views for OpenideaMise {
    fn render(
        &mut self,
        view: &str,
        _viewport: Option<Viewport>,
        ctx: &Invocation,
    ) -> Result<View, Error> {
        if view != VIEW {
            return Err(Error::NotFound(view.into()));
        }
        let (_, root) = Self::root(ctx)?;
        let snapshot = self.snapshot(&root);
        let start = snapshot_message(&snapshot)
            .to_string()
            .replace('<', "\\u003c");
        let page = PAGE.replacen(r#""__START__""#, &start, 1);
        Ok(View::new(Body::Html(page)).button(Button::new("refresh", "Refresh").icon("refresh")))
    }

    fn handle(&mut self, view: &str, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        if view != VIEW {
            return Err(Error::NotFound(view.into()));
        }
        match event {
            ViewEvent::Button(button) if button == "refresh" => self.post_snapshot(ctx, true),
            ViewEvent::Message(message) => {
                let Ok(value) = serde_json::from_str::<Value>(&message) else {
                    return Ok(());
                };
                match value.get("op").and_then(Value::as_str) {
                    Some("ready") => self.post_snapshot(ctx, false),
                    Some("refresh") => self.post_snapshot(ctx, true),
                    Some("runTask") => {
                        let name = value.get("task").and_then(Value::as_str);
                        match self.run_task(ctx, name) {
                            Ok(_) => host::post_view_message(
                                VIEW,
                                &json!({"op":"runStarted","task":name.unwrap_or_default()})
                                    .to_string(),
                            ),
                            Err(error) => post_error(&error.to_string()),
                        }
                    }
                    Some("revealEnvironment") => {
                        let Some(name) = value.get("name").and_then(Value::as_str) else {
                            return Ok(());
                        };
                        let (_, root) = Self::root(ctx)?;
                        let snapshot = self.snapshot(&root);
                        let Some(entry) =
                            snapshot.environment.iter().find(|entry| entry.name == name)
                        else {
                            return post_error("environment value is no longer available");
                        };
                        host::post_view_message(
                            VIEW,
                            &json!({"op":"environmentValue","name":entry.name,"value":entry.value})
                                .to_string(),
                        )
                    }
                    _ => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }
}

impl RunConfigurations for OpenideaMise {
    fn configurations(&mut self, project_root: &str) -> Vec<PluginRunConfiguration> {
        let snapshot = self.snapshot(project_root);
        snapshot
            .tasks
            .iter()
            .map(|task| run_configuration(project_root, task))
            .collect()
    }
}

fn run_configuration(project_root: &str, task: &MiseTask) -> PluginRunConfiguration {
    PluginRunConfiguration {
        id: task_configuration_id(&task.name),
        name: format!("Mise: {}", task.name),
        program: "mise".into(),
        args: vec!["run".into(), task.name.clone()],
        cwd: Some(project_root.into()),
        test: false,
        framework: None,
    }
}

fn configure_run_target(project: &openidea_plugin::Project) -> Result<(), Error> {
    if !host::setting_bool("registerRunTarget").unwrap_or(true) {
        host::unregister_run_target(project, "mise.exec");
        return Ok(());
    }
    host::register_run_target(
        project,
        &PluginRunTarget {
            id: "mise.exec".into(),
            name: "Mise".into(),
            program: "mise".into(),
            args: vec!["exec".into(), "--".into()],
            workdir_args: Vec::new(),
            container_root: None,
            commands: Vec::new(),
        },
    )
}

fn snapshot_message(snapshot: &Snapshot) -> Value {
    let redact = host::setting_bool("redactEnvironment").unwrap_or(true);
    let interval = host::setting_number("autoRefreshSeconds")
        .unwrap_or(30.0)
        .max(0.0) as u64;
    snapshot.view_json(redact, interval)
}

fn post_error(message: &str) -> Result<(), Error> {
    host::post_view_message(VIEW, &json!({"op":"error","message":message}).to_string())
}

fn is_config_path(path: &str) -> bool {
    let file_name = path
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    CONFIG_FILES.iter().any(|candidate| file_name == *candidate)
}

register_plugin!(
    OpenideaMise,
    views,
    run_configurations,
    providers,
    hover,
    language_features,
    injections,
    indexing,
);

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{ProcessOutput, mock::MockHost};

    fn host_with_mise() -> MockHost {
        let host = MockHost::install().with_capabilities(&[
            "project.read",
            "storage",
            "events",
            "process:mise",
        ]);
        host.set_project_root(Some("/project"));
        host.on_process("mise", |args, _| {
            let stdout = match args {
                [a, b, c] if a == "tasks" && b == "ls" && c == "--json" => {
                    r#"[{"name":"build","aliases":[],"description":"Build","source":"/project/mise.toml","depends":["lint"],"depends_post":[],"wait_for":[],"dir":"/project","hide":false}]"#
                }
                [a, b] if a == "ls" && b == "--json" => {
                    r#"{"node":[{"version":"22.0.0","requested_version":"22","source":{"path":"/project/mise.toml"},"installed":true,"active":true}]}"#
                }
                [a, b] if a == "env" && b == "--json" => {
                    r#"{"API_TOKEN":"secret","PUBLIC_VALUE":"visible"}"#
                }
                _ => return Err(Error::Invalid(format!("unexpected mise args: {args:?}"))),
            };
            Ok(ProcessOutput { status: Some(0), stdout: stdout.into(), stderr: String::new() })
        });
        host
    }

    #[test]
    fn refresh_builds_snapshot_cache_and_dynamic_run_configuration() {
        let host = host_with_mise();
        let mut plugin = OpenideaMise::default();
        let snapshot = plugin.refresh(&host.invocation()).unwrap();
        assert_eq!(snapshot.tasks[0].name, "build");
        assert_eq!(snapshot.tools[0].name, "node");
        assert!(snapshot.environment[0].sensitive);

        let configs = plugin.configurations("/project");
        assert_eq!(configs[0].name, "Mise: build");
        assert_eq!(configs[0].program, "mise");
        assert_eq!(configs[0].args, ["run", "build"]);

        let mut reloaded = OpenideaMise::default();
        assert_eq!(reloaded.configurations("/project")[0].name, "Mise: build");
        assert!(reloaded.snapshot("/project").environment.is_empty());
    }

    #[test]
    fn view_never_embeds_a_redacted_secret() {
        let host = host_with_mise();
        let mut plugin = OpenideaMise::default();
        plugin.refresh(&host.invocation()).unwrap();
        let View {
            body: Body::Html(page),
            ..
        } = plugin.render(VIEW, None, &host.invocation()).unwrap()
        else {
            panic!("expected html")
        };
        assert!(!page.contains("\"secret\""));
        assert!(page.contains("••••••••"));
    }

    #[test]
    fn run_task_starts_its_owned_configuration() {
        let host = host_with_mise();
        let mut plugin = OpenideaMise::default();
        let snapshot = plugin.refresh(&host.invocation()).unwrap();
        host.set_run_configurations(
            snapshot
                .tasks
                .iter()
                .map(|task| run_configuration("/project", task))
                .collect(),
        );
        plugin.run_task(&host.invocation(), Some("build")).unwrap();
        assert_eq!(
            host.configuration_runs()[0].1,
            task_configuration_id("build")
        );
    }

    #[test]
    fn config_names_are_detected_on_unix_and_windows() {
        assert!(is_config_path("/project/.mise.toml"));
        assert!(is_config_path(r"C:\project\mise.local.toml"));
        assert!(!is_config_path("/project/Cargo.toml"));
    }

    #[test]
    fn denied_process_capability_degrades_to_section_errors() {
        let host = MockHost::install().with_capabilities(&["project.read", "storage", "events"]);
        host.set_project_root(Some("/project"));
        let snapshot = OpenideaMise::default().refresh(&host.invocation()).unwrap();
        assert_eq!(snapshot.errors.len(), 3);
        assert!(
            snapshot
                .errors
                .iter()
                .all(|error| error.contains("permission to run mise"))
        );
    }
}
