mod graph;
mod mise;
mod model;
mod state;
mod toml_support;

use crate::model::{MiseTask, Snapshot, task_configuration_id};
use openidea_plugin::views::{View, ViewEvent, Viewport, Views};
use openidea_plugin::{
    Error, Event, Invocation, NotificationLevel, Plugin, PluginRunConfiguration, PluginRunTarget,
    RunConfigurations, host, register_plugin,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const VIEW: &str = "mise";
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
        let task_name = match task_name {
            Some(name) => validate_task_name(name)?.to_owned(),
            None => {
                let snapshot = self.snapshot(&root);
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
                let name = snapshot
                    .tasks
                    .iter()
                    .find(|task| task.name == chosen)
                    .map(|task| task.name.as_str())
                    .ok_or_else(|| Error::NotFound(format!("mise task {chosen}")))?;
                validate_task_name(name)?.to_owned()
            }
        };
        match host::start_run(&task_configuration_id(&task_name), &[], false) {
            Ok(operation) => Ok(Some(operation)),
            Err(run_error) => {
                let args = vec!["run".into(), task_name.clone()];
                host::open_terminal(&format!("Mise: {task_name}"), "mise", &args).map_err(
                    |terminal_error| {
                        Error::Failed(format!(
                            "could not run mise task {task_name}: Run tool: {run_error}; terminal fallback: {terminal_error}"
                        ))
                    },
                )?;
                host::notify(
                    NotificationLevel::Info,
                    "Mise task opened in terminal",
                    &format!(
                        "The Run configuration was unavailable, so `{task_name}` was started in a terminal."
                    ),
                );
                Ok(Some(format!(
                    "terminal:{}",
                    task_configuration_id(&task_name)
                )))
            }
        }
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
            "run-task" => match self.run_task(ctx, arg) {
                Ok(result) => Ok(result),
                Err(error) => {
                    notify_run_error(arg, &error);
                    Err(error)
                }
            },
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
        _ctx: &Invocation,
    ) -> Result<View, Error> {
        if view != VIEW {
            return Err(Error::NotFound(view.into()));
        }
        Ok(View::message("Mise is shown by the plugin's web view."))
    }

    fn handle(&mut self, view: &str, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        if view != VIEW {
            return Err(Error::NotFound(view.into()));
        }
        match event {
            ViewEvent::Message(message) => {
                let Ok(value) = serde_json::from_str::<Value>(&message) else {
                    return Ok(());
                };
                match value.get("op").and_then(Value::as_str) {
                    Some("ready") => self.post_snapshot(ctx, true),
                    Some("refresh") => self.post_snapshot(ctx, true),
                    Some("runTask") => {
                        let name = value.get("task").and_then(Value::as_str);
                        match self.run_task(ctx, name) {
                            Ok(_) => host::post_view_message(
                                VIEW,
                                &json!({"op":"runStarted","task":name.unwrap_or_default()})
                                    .to_string(),
                            ),
                            Err(error) => {
                                notify_run_error(name, &error);
                                post_error(&error.to_string())
                            }
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

fn notify_run_error(task: Option<&str>, error: &Error) {
    let subject = task
        .filter(|name| !name.is_empty())
        .map(|name| format!("task `{name}`"))
        .unwrap_or_else(|| "task".into());
    host::notify(
        NotificationLevel::Error,
        "Could not run Mise task",
        &format!("Failed to start {subject}: {error}"),
    );
}

fn validate_task_name(name: &str) -> Result<&str, Error> {
    if name.is_empty()
        || name.len() > 256
        || name
            .chars()
            .any(|character| character == '\0' || character == '\r' || character == '\n')
    {
        return Err(Error::Invalid("invalid mise task name".into()));
    }
    Ok(name)
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
    fn snapshot_messages_never_expose_a_redacted_secret() {
        let host = host_with_mise();
        let mut plugin = OpenideaMise::default();
        let snapshot = plugin.refresh(&host.invocation()).unwrap();
        let message = snapshot_message(&snapshot).to_string();
        assert!(!message.contains("\"secret\""));
        assert!(message.contains("••••••••"));
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
    fn run_task_falls_back_to_a_terminal_when_configuration_is_unavailable() {
        let host = host_with_mise();
        let mut plugin = OpenideaMise::default();
        plugin.refresh(&host.invocation()).unwrap();

        let operation = plugin
            .run_task(&host.invocation(), Some("build"))
            .unwrap()
            .unwrap();

        assert!(operation.starts_with("terminal:"));
        assert_eq!(host.terminals().len(), 1);
        assert_eq!(host.terminals()[0].program, "mise");
        assert_eq!(host.terminals()[0].args, ["run", "build"]);
        assert_eq!(
            host.notifications()[0].title,
            "Mise task opened in terminal"
        );
    }

    #[test]
    fn run_command_notifies_when_both_run_paths_fail() {
        let host = MockHost::install().with_capabilities(&["project.read", "storage", "events"]);
        host.set_project_root(Some("/project"));
        let mut plugin = OpenideaMise::default();

        let error = plugin
            .run_command("run-task", &host.invocation(), Some("build"))
            .unwrap_err();

        assert!(error.to_string().contains("could not run mise task build"));
        assert_eq!(host.notifications()[0].title, "Could not run Mise task");
    }

    #[test]
    fn web_view_ready_refreshes_and_posts_a_snapshot() {
        let host = host_with_mise();
        let mut plugin = OpenideaMise::default();
        plugin
            .handle(
                VIEW,
                ViewEvent::Message(r#"{"op":"ready"}"#.into()),
                &host.invocation(),
            )
            .unwrap();

        let messages = host.view_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].0, VIEW);
        let snapshot: Value = serde_json::from_str(&messages[0].1).unwrap();
        assert_eq!(snapshot["tasks"][0]["name"], "build");
        assert_eq!(snapshot["tools"][0]["name"], "node");
    }

    #[test]
    fn packaged_web_view_has_no_duplicate_mise_heading() {
        const PAGE: &str = include_str!("../web/index.html");
        const MANIFEST: &str = include_str!("../plugin.toml");

        assert!(PAGE.contains("<!doctype html>"));
        assert!(PAGE.contains("globalThis.openidea"));
        assert!(PAGE.contains(r#"id="refresh""#));
        assert!(!PAGE.contains("<strong>Mise</strong>"));
        assert!(MANIFEST.contains(r#"web = "web/index.html""#));
        assert!(MANIFEST.contains(r#"header = "none""#));
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
