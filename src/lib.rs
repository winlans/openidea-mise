mod graph;
mod mise;
mod model;
mod state;
mod toml_support;

use crate::model::{MiseTask, Snapshot, task_configuration_id};
use openidea_plugin::views::{View, ViewEvent, Viewport, Views};
use openidea_plugin::{
    Error, Event, Invocation, NotificationLevel, Plugin, PluginRunConfiguration, PluginRunTarget,
    RunConfigurations, RunTargetLaunch, RunTargetValidation, host, register_plugin,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const VIEW: &str = "mise";

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

    fn root_and_context(
        ctx: &Invocation,
    ) -> Result<(openidea_plugin::Project, String, String), Error> {
        let (project, root) = Self::root(ctx)?;
        let context = if host::setting_bool("followEditorContext").unwrap_or(true) {
            ctx.editor()
                .map(|editor| context_directory(&root, &editor.path()))
                .unwrap_or_else(|| root.clone())
        } else {
            root.clone()
        };
        Ok((project, root, context))
    }

    fn snapshot(&mut self, root: &str, context: &str) -> Snapshot {
        let key = snapshot_key(root, context);
        if let Some(snapshot) = self.snapshots.get(&key) {
            return snapshot.clone();
        }
        let snapshot = state::load(root, context)
            .ok()
            .flatten()
            .unwrap_or_else(|| Snapshot {
                project: root.to_owned(),
                context: context.to_owned(),
                errors: vec!["Refresh to load Mise data for this project.".into()],
                ..Default::default()
            });
        self.snapshots.insert(key, snapshot.clone());
        snapshot
    }

    fn snapshot_for_run_configurations(&mut self, root: &str) -> Snapshot {
        let mut snapshot = self.snapshot(root, root);
        let Some(project) = host::project_of(root) else {
            return snapshot;
        };

        // Run configurations may be requested before the Mise tool window has
        // ever opened. Refresh just the inexpensive task list here so the
        // first Run/Debug chooser already contains the project's Mise tasks.
        // A transient CLI failure keeps the last good cache available.
        match mise::tasks(&project, root) {
            Ok(tasks) => {
                snapshot.project = root.to_owned();
                snapshot.context = root.to_owned();
                snapshot.tasks = tasks;
                snapshot.trust_required = false;
                let _ = state::save(&snapshot);
                self.snapshots
                    .insert(snapshot_key(root, root), snapshot.clone());
            }
            Err(error) if mise::requires_trust(&error) => {
                snapshot.tasks.clear();
                snapshot.trust_required = true;
            }
            Err(_) => {}
        }
        let _ = configure_run_target(&project);
        let _ = configure_file_icons(&project, &snapshot);
        snapshot
    }

    fn refresh_context(
        &mut self,
        project: &openidea_plugin::Project,
        root: &str,
        context: &str,
    ) -> Snapshot {
        let mut snapshot = self.snapshot(root, context);
        snapshot.project = root.to_owned();
        snapshot.context = context.to_owned();
        snapshot.errors.clear();
        snapshot.trust_required = false;

        match mise::tasks(project, context) {
            Ok(tasks) => snapshot.tasks = tasks,
            Err(error) if mise::requires_trust(&error) => {
                snapshot.tasks.clear();
                snapshot.tools.clear();
                snapshot.configs.clear();
                snapshot.environment.clear();
                snapshot.trust_required = true;
                snapshot.errors.push(
                    "Mise configuration is not trusted. Trust it to load project tasks, tools, and environment."
                        .into(),
                );
            }
            Err(error) => snapshot.errors.push(format!("Tasks: {error}")),
        }
        if !snapshot.trust_required {
            match mise::tools(project, context) {
                Ok(tools) => snapshot.tools = tools,
                Err(error) => snapshot.errors.push(format!("Tools: {error}")),
            }
            match mise::environment(project, context) {
                Ok(environment) => snapshot.environment = environment,
                Err(error) => {
                    snapshot.environment.clear();
                    snapshot.errors.push(format!("Environment: {error}"));
                }
            }
            match mise::config_files(project, context, root) {
                Ok(configs) => snapshot.configs = configs,
                Err(error) => snapshot.errors.push(format!("Configuration: {error}")),
            }
        }

        if let Err(error) = state::save(&snapshot) {
            snapshot.errors.push(format!("Cache: {error}"));
        }
        if let Err(error) = configure_run_target(project)
            && !matches!(error, Error::Denied(_))
        {
            snapshot.errors.push(format!("Run target: {error}"));
        }
        if let Err(error) = configure_file_icons(project, &snapshot)
            && !matches!(error, Error::Denied(_))
        {
            snapshot.errors.push(format!("File icons: {error}"));
        }
        self.snapshots
            .insert(snapshot_key(root, context), snapshot.clone());
        snapshot
    }

    fn refresh(&mut self, ctx: &Invocation) -> Result<Snapshot, Error> {
        let (project, root, context) = Self::root_and_context(ctx)?;
        Ok(self.refresh_context(&project, &root, &context))
    }

    fn post_snapshot(
        &mut self,
        ctx: &Invocation,
        refresh: bool,
        session_id: Option<&str>,
        request_id: Option<u64>,
    ) -> Result<(), Error> {
        let (_, root, context) = Self::root_and_context(ctx)?;
        let snapshot = if refresh {
            self.refresh(ctx)?
        } else {
            self.snapshot(&root, &context)
        };
        host::post_view_message(
            VIEW,
            &snapshot_message(&snapshot, session_id, request_id).to_string(),
        )
    }

    fn trust_configuration(
        &mut self,
        ctx: &Invocation,
        session_id: Option<&str>,
        request_id: Option<u64>,
    ) -> Result<(), Error> {
        let (project, _, context) = Self::root_and_context(ctx)?;
        mise::trust(&project, &context).map_err(Error::Failed)?;
        self.post_snapshot(ctx, true, session_id, request_id)
    }

    fn run_task(
        &mut self,
        ctx: &Invocation,
        task_name: Option<&str>,
    ) -> Result<Option<String>, Error> {
        let (_, root, context) = Self::root_and_context(ctx)?;
        let task_name = match task_name {
            Some(name) => validate_task_name(name)?.to_owned(),
            None => {
                let snapshot = self.snapshot(&root, &context);
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
        if same_path(&root, &context) {
            match host::start_run(&task_configuration_id(&task_name), &[], false) {
                Ok(operation) => return Ok(Some(operation)),
                Err(run_error) => {
                    return self.open_task_terminal(&task_name, &root, &context, Some(&run_error));
                }
            }
        }
        self.open_task_terminal(&task_name, &root, &context, None)
    }

    fn open_task_terminal(
        &self,
        task_name: &str,
        root: &str,
        context: &str,
        run_error: Option<&Error>,
    ) -> Result<Option<String>, Error> {
        let mut args = Vec::new();
        if !same_path(root, context) {
            args.extend(["-C".into(), context.into()]);
        }
        args.extend(["run".into(), task_name.into()]);
        host::open_terminal(&format!("Mise: {task_name}"), "mise", &args).map_err(
            |terminal_error| {
                Error::Failed(match run_error {
                    Some(run_error) => format!(
                        "could not run mise task {task_name}: Run tool: {run_error}; terminal fallback: {terminal_error}"
                    ),
                    None => format!(
                        "could not run mise task {task_name} in {context}: {terminal_error}"
                    ),
                })
            },
        )?;
        host::notify(
            NotificationLevel::Info,
            "Mise task opened in terminal",
            &if same_path(root, context) {
                format!(
                    "The Run configuration was unavailable, so `{task_name}` was started in a terminal."
                )
            } else {
                format!("`{task_name}` was started in the active Mise context `{context}`.")
            },
        );
        Ok(Some(format!(
            "terminal:{}",
            task_configuration_id(task_name)
        )))
    }

    fn invalidate(&mut self, ctx: &Invocation) {
        self.invalidate_memory(ctx);
        let Ok((_, root)) = Self::root(ctx) else {
            return;
        };
        state::delete(&root);
    }

    fn invalidate_memory(&mut self, ctx: &Invocation) {
        let Ok((_, root)) = Self::root(ctx) else {
            return;
        };
        let prefix = format!("{}\0", normalize_path(&root));
        self.snapshots.retain(|key, _| !key.starts_with(&prefix));
    }

    fn post_invalidation(&self, ctx: &Invocation, refresh: bool) {
        let Ok((_, root)) = Self::root(ctx) else {
            return;
        };
        let _ = post_message(
            json!({"op":"invalidate","project":root,"refresh":refresh}),
            None,
            None,
        );
    }
}

impl Plugin for OpenideaMise {
    fn activate(&mut self, _ctx: &openidea_plugin::Context) -> Result<(), Error> {
        let globs = [
            "**/mise*.toml",
            "**/.mise*.toml",
            "**/mise/**/*.toml",
            "**/.mise/**/*.toml",
            "**/.config/mise*.toml",
            "**/.config/mise/**/*.toml",
            "**/.miserc*.toml",
            "**/.tool-versions",
            "**/mise-tasks/**",
            "**/.mise-tasks/**",
            "**/mise/tasks/**",
            "**/.mise/tasks/**",
            "**/.config/mise/tasks/**",
        ];
        host::subscribe_paths("file-saved", &globs)?;
        host::subscribe_paths("files-changed", &globs)?;
        host::subscribe_paths("document-changed", &globs)?;
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
                self.post_snapshot(ctx, true, None, None)?;
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
                Event::FileSaved(path) if is_hierarchy_path(path) => {
                    self.invalidate(ctx);
                    self.refresh_icons(ctx);
                    self.post_invalidation(ctx, true);
                }
                Event::FilesChanged(changes)
                    if changes
                        .created
                        .iter()
                        .chain(&changes.modified)
                        .chain(&changes.removed)
                        .any(|path| is_hierarchy_path(path)) =>
                {
                    self.invalidate(ctx);
                    self.refresh_icons(ctx);
                    self.post_invalidation(ctx, true);
                }
                Event::DocumentChanged(change) if is_hierarchy_path(&change.path) => {
                    self.invalidate_memory(ctx);
                    self.post_invalidation(ctx, false);
                }
                Event::Other { kind, .. } if kind == "settings-changed" => {
                    if let Ok((project, _)) = Self::root(ctx) {
                        let _ = configure_run_target(&project);
                    }
                }
                _ => {}
            }
        }
    }
}

impl OpenideaMise {
    fn refresh_icons(&mut self, ctx: &Invocation) {
        let Ok((project, root)) = Self::root(ctx) else {
            return;
        };
        let snapshot = self.snapshot(&root, &root);
        let _ = configure_file_icons(&project, &snapshot);
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
                let session_id = message_session_id(&value);
                let request_id = message_request_id(&value);
                match value.get("op").and_then(Value::as_str) {
                    Some(operation @ ("ready" | "refresh" | "sync")) => {
                        let refresh = operation != "sync";
                        match self.post_snapshot(ctx, refresh, session_id, request_id) {
                            Ok(()) => Ok(()),
                            Err(error) => post_error(&error.to_string(), session_id, request_id),
                        }
                    }
                    Some("trust") => match self.trust_configuration(ctx, session_id, request_id) {
                        Ok(()) => Ok(()),
                        Err(error) => post_error(&error.to_string(), session_id, request_id),
                    },
                    Some("runTask") => {
                        let name = value.get("task").and_then(Value::as_str);
                        match self.run_task(ctx, name) {
                            Ok(_) => post_message(
                                json!({"op":"runStarted","task":name.unwrap_or_default()}),
                                session_id,
                                request_id,
                            ),
                            Err(error) => {
                                notify_run_error(name, &error);
                                post_error(&error.to_string(), session_id, request_id)
                            }
                        }
                    }
                    Some("revealEnvironment") => {
                        let Some(name) = value.get("name").and_then(Value::as_str) else {
                            return Ok(());
                        };
                        let (_, root, context) = Self::root_and_context(ctx)?;
                        let snapshot = self.snapshot(&root, &context);
                        let Some(entry) =
                            snapshot.environment.iter().find(|entry| entry.name == name)
                        else {
                            return post_error(
                                "environment value is no longer available",
                                session_id,
                                request_id,
                            );
                        };
                        post_message(
                            json!({"op":"environmentValue","name":entry.name,"value":entry.value}),
                            session_id,
                            request_id,
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
        let snapshot = self.snapshot_for_run_configurations(project_root);
        snapshot
            .tasks
            .iter()
            .map(|task| run_configuration(project_root, task))
            .collect()
    }
}

impl RunTargetValidation for OpenideaMise {
    fn validate(
        &mut self,
        project: &str,
        target_id: &str,
        launch: &RunTargetLaunch,
    ) -> Result<(), String> {
        if target_id != "mise.exec" {
            return Ok(());
        }
        let Some(project_handle) = host::project_of(project) else {
            return Err("The OpenIDEA project is no longer open.".into());
        };
        let root = project_handle
            .root()
            .map_err(|error| format!("Could not read the project root: {error}"))?;
        if !path_is_within(&launch.cwd, &root) {
            return Err(format!(
                "Mise can only run inside this project. `{}` is outside `{root}`.",
                launch.cwd
            ));
        }
        mise::validate_context(&project_handle, &launch.cwd).map_err(|error| {
            if mise::requires_trust(&error) {
                "The Mise configuration for this working directory is not trusted. Open the Mise tool window and choose Trust before running.".into()
            } else {
                format!("Mise could not validate this working directory: {error}")
            }
        })
    }
}

fn run_configuration(project_root: &str, task: &MiseTask) -> PluginRunConfiguration {
    PluginRunConfiguration {
        id: task_configuration_id(&task.name),
        name: task.name.clone(),
        program: "mise".into(),
        args: vec!["run".into(), task.name.clone()],
        cwd: Some(project_root.into()),
        test: false,
        framework: None,
    }
}

fn snapshot_key(root: &str, context: &str) -> String {
    format!("{}\0{}", comparable_path(root), comparable_path(context))
}

fn context_directory(root: &str, path: &str) -> String {
    let root = normalize_path(root);
    let path = normalize_path(path);
    let root = root.trim_end_matches('/');
    let comparable_root = comparable_path(root);
    let comparable_path = comparable_path(&path);
    if comparable_path != comparable_root
        && !comparable_path.starts_with(&format!("{comparable_root}/"))
    {
        return root.to_owned();
    }
    let Some((directory, _)) = path.rsplit_once('/') else {
        return root.to_owned();
    };
    if directory.len() < root.len() {
        root.to_owned()
    } else {
        directory.to_owned()
    }
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").trim_end_matches('/').to_owned()
}

fn same_path(left: &str, right: &str) -> bool {
    comparable_path(left) == comparable_path(right)
}

fn path_is_within(path: &str, root: &str) -> bool {
    let path = comparable_path(path);
    let root = comparable_path(root);
    path == root || path.starts_with(&format!("{root}/"))
}

fn comparable_path(path: &str) -> String {
    let normalized = normalize_path(path);
    if normalized.as_bytes().get(1) == Some(&b':') || normalized.starts_with("//") {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

fn configure_run_target(project: &openidea_plugin::Project) -> Result<(), Error> {
    if !host::setting_bool("registerRunTarget").unwrap_or(false) {
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

const FILE_TASK_DIRECTORIES: [&str; 5] = [
    "mise-tasks",
    ".mise-tasks",
    "mise/tasks",
    ".mise/tasks",
    ".config/mise/tasks",
];

fn configure_file_icons(
    project: &openidea_plugin::Project,
    snapshot: &Snapshot,
) -> Result<(), Error> {
    let root = project.root()?;
    let mut configs: BTreeMap<String, String> = BTreeMap::new();
    for config in &snapshot.configs {
        if !config.path.is_empty() && path_is_within(&config.path, &root) {
            configs.insert(config.path.clone(), "mise-config".into());
        }
    }
    let mut tasks: BTreeMap<String, String> = BTreeMap::new();
    for path in project.find_files("**/*", 10_000).unwrap_or_default() {
        if is_config_path(&path) || is_mise_marker_path(&path) {
            configs.insert(path.clone(), "mise-config".into());
        }
        if is_file_task_path(&path) {
            if let Some(directory_path) = file_task_directory(&path) {
                tasks.insert(directory_path, "mise-task".into());
            }
            tasks.insert(path, "mise-task".into());
        }
    }
    for directory in FILE_TASK_DIRECTORIES {
        let path = format!("{}/{}", root.trim_end_matches(['/', '\\']), directory);
        if project.exists(&path).unwrap_or(false) {
            tasks.insert(path, "mise-task".into());
        }
    }
    for task in &snapshot.tasks {
        if !task.source.is_empty()
            && path_is_within(&task.source, &root)
            && !is_config_path(&task.source)
            && !is_mise_marker_path(&task.source)
            && project.exists(&task.source).unwrap_or(false)
        {
            tasks.insert(task.source.clone(), "mise-task".into());
            if let Some(directory) = file_task_directory(&task.source) {
                tasks.insert(directory, "mise-task".into());
            }
        }
    }

    host::set_file_icons(
        "mise-config-files",
        &configs.into_iter().take(10_000).collect::<Vec<_>>(),
    )?;
    host::set_file_icons(
        "mise-file-tasks",
        &tasks.into_iter().take(10_000).collect::<Vec<_>>(),
    )
}

fn file_task_directory(path: &str) -> Option<String> {
    let normalized = normalize_path(path);
    let comparable = comparable_path(&normalized);
    let mut best: Option<(usize, usize)> = None;
    for directory in FILE_TASK_DIRECTORIES {
        let marker = format!("/{directory}/");
        if let Some(index) = comparable.rfind(&marker) {
            let end = index + marker.len() - 1;
            if best.is_none_or(|(best_index, _)| index > best_index) {
                best = Some((index, end));
            }
        }
    }
    best.map(|(_, end)| normalized[..end].to_owned())
}

pub(crate) fn is_file_task_path(path: &str) -> bool {
    file_task_directory(path).is_some()
}

pub(crate) fn file_task_name(path: &str) -> Option<String> {
    let normalized = normalize_path(path);
    let directory = file_task_directory(&normalized)?;
    let relative = normalized
        .strip_prefix(&format!("{directory}/"))?
        .trim_matches('/');
    if relative.is_empty() {
        return None;
    }
    let mut parts: Vec<_> = relative.split('/').map(str::to_owned).collect();
    if let Some(file_name) = parts.last_mut()
        && let Some((stem, _)) = file_name.rsplit_once('.')
        && !stem.is_empty()
    {
        *file_name = stem.to_owned();
    }
    Some(parts.join(":"))
}

fn snapshot_message(
    snapshot: &Snapshot,
    session_id: Option<&str>,
    request_id: Option<u64>,
) -> Value {
    let redact = host::setting_bool("redactEnvironment").unwrap_or(true);
    let interval = host::setting_number("autoRefreshSeconds")
        .unwrap_or(30.0)
        .max(0.0) as u64;
    with_routing(snapshot.view_json(redact, interval), session_id, request_id)
}

fn message_session_id(value: &Value) -> Option<&str> {
    value
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 128)
}

fn message_request_id(value: &Value) -> Option<u64> {
    value.get("requestId").and_then(Value::as_u64)
}

fn with_routing(mut message: Value, session_id: Option<&str>, request_id: Option<u64>) -> Value {
    if let Some(object) = message.as_object_mut() {
        if let Some(session_id) = session_id {
            object.insert("sessionId".into(), Value::String(session_id.into()));
        }
        if let Some(request_id) = request_id {
            object.insert("requestId".into(), Value::Number(request_id.into()));
        }
    }
    message
}

fn post_message(
    message: Value,
    session_id: Option<&str>,
    request_id: Option<u64>,
) -> Result<(), Error> {
    host::post_view_message(
        VIEW,
        &with_routing(message, session_id, request_id).to_string(),
    )
}

fn post_error(
    message: &str,
    session_id: Option<&str>,
    request_id: Option<u64>,
) -> Result<(), Error> {
    post_message(
        json!({"op":"error","message":message}),
        session_id,
        request_id,
    )
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
    let path = normalize_path(path).to_ascii_lowercase();
    let file_name = path.rsplit('/').next().unwrap_or_default();
    let parent = path
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("");
    if file_name.ends_with(".toml")
        && (parent.contains("/mise/conf.d")
            || parent.contains("/.mise/conf.d")
            || parent.contains("/.config/mise/conf.d"))
    {
        return true;
    }
    let unhidden = file_name.strip_prefix('.').unwrap_or(file_name);
    if unhidden == "mise.toml"
        || unhidden == "mise.local.toml"
        || (unhidden.starts_with("mise.") && unhidden.ends_with(".toml"))
    {
        return true;
    }
    if !(file_name == "config.toml"
        || file_name == "config.local.toml"
        || (file_name.starts_with("config.") && file_name.ends_with(".toml")))
    {
        return false;
    }
    parent.ends_with("/mise") || parent.ends_with("/.mise") || parent.ends_with("/.config/mise")
}

fn is_hierarchy_path(path: &str) -> bool {
    is_config_path(path) || is_file_task_path(path) || is_mise_marker_path(path)
}

fn is_mise_marker_path(path: &str) -> bool {
    matches!(
        normalize_path(path)
            .to_ascii_lowercase()
            .rsplit('/')
            .next()
            .unwrap_or_default(),
        ".miserc.toml"
            | ".miserc.local.toml"
            | "miserc.toml"
            | "miserc.local.toml"
            | ".tool-versions"
    )
}

register_plugin!(
    OpenideaMise,
    views,
    run_configurations,
    run_target_validation,
    providers,
    hover,
    language_features,
    injections,
    indexing,
    diagnostics,
    semantic_tokens,
);

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{ProcessOutput, mock::MockHost};
    use std::{cell::Cell, rc::Rc};

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
                [a, b, c] if a == "config" && b == "ls" && c == "--json" => {
                    r#"[{"path":"/project/mise.toml","tools":["node"]}]"#
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
        assert_eq!(configs[0].name, "build");
        assert_eq!(configs[0].program, "mise");
        assert_eq!(configs[0].args, ["run", "build"]);

        let mut reloaded = OpenideaMise::default();
        assert_eq!(reloaded.configurations("/project")[0].name, "build");
        assert!(
            reloaded
                .snapshot("/project", "/project")
                .environment
                .is_empty()
        );
    }

    #[test]
    fn run_configurations_load_tasks_before_the_tool_window_opens() {
        let host = host_with_mise();
        let mut plugin = OpenideaMise::default();

        let configs = plugin.configurations("/project");

        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "build");
        assert_eq!(configs[0].args, ["run", "build"]);
        assert_eq!(host.process_runs().len(), 1);
        assert_eq!(host.process_runs()[0].args, ["tasks", "ls", "--json"]);
    }

    #[test]
    fn mise_run_target_is_opt_in_and_is_withdrawn_when_disabled() {
        let host = host_with_mise();
        let project = host.invocation().project().unwrap();

        host.set_setting("registerRunTarget", "true");
        configure_run_target(&project).unwrap();
        assert_eq!(host.run_targets().len(), 1);

        host.set_setting("registerRunTarget", "false");
        configure_run_target(&project).unwrap();
        assert!(host.run_targets().is_empty());
    }

    #[test]
    fn mise_run_target_validation_checks_project_scope_and_trust() {
        let host = host_with_mise();
        host.on_process("mise", |args, _| match args {
            [flag, context, a, b, c]
                if flag == "-C"
                    && context == "/project/backend"
                    && a == "config"
                    && b == "ls"
                    && c == "--json" =>
            {
                Ok(ProcessOutput {
                    status: Some(0),
                    stdout: "[]".into(),
                    stderr: String::new(),
                })
            }
            _ => Err(Error::Invalid(format!("unexpected mise args: {args:?}"))),
        });
        let mut plugin = OpenideaMise::default();
        let launch = RunTargetLaunch {
            operation: openidea_plugin::RunTargetOperation::Run,
            program: "npm".into(),
            cwd: "/project/backend".into(),
        };

        assert!(
            host.validate_run_target(&mut plugin, "mise.exec", &launch)
                .is_ok()
        );
        assert_eq!(
            host.process_runs().last().unwrap().args,
            ["-C", "/project/backend", "config", "ls", "--json"]
        );

        let outside = RunTargetLaunch {
            cwd: "/other/project".into(),
            ..launch.clone()
        };
        assert!(
            host.validate_run_target(&mut plugin, "mise.exec", &outside)
                .unwrap_err()
                .contains("outside")
        );

        host.on_process("mise", |_, _| {
            Ok(ProcessOutput {
                status: Some(1),
                stdout: String::new(),
                stderr: "Config files in /project/mise.toml are not trusted.".into(),
            })
        });
        assert!(
            host.validate_run_target(&mut plugin, "mise.exec", &launch)
                .unwrap_err()
                .contains("not trusted")
        );
    }

    #[test]
    fn dynamic_icons_cover_configs_task_directories_and_file_tasks() {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        host.set_project_root(Some("/project"));
        host.add_file("mise.toml", "[tools]\nnode = \"22\"\n");
        host.add_file(".mise/tasks/build", "#!/bin/sh\n");
        let project = host.invocation().project().unwrap();
        let snapshot = Snapshot {
            project: "/project".into(),
            context: "/project".into(),
            configs: vec![crate::model::MiseConfigFile {
                path: "/home/user/.config/mise/config.toml".into(),
                ..Default::default()
            }],
            tasks: vec![
                MiseTask {
                    name: "build".into(),
                    source: "/project/.mise/tasks/build".into(),
                    ..Default::default()
                },
                MiseTask {
                    name: "configured".into(),
                    source: "/project/mise.toml".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        configure_file_icons(&project, &snapshot).unwrap();

        let icons = host.file_icons();
        assert_eq!(
            icons
                .get(&("mise-config-files".into(), "/project/mise.toml".into()))
                .map(|value| value.0.as_str()),
            Some("mise-config")
        );
        assert_eq!(
            icons
                .get(&(
                    "mise-file-tasks".into(),
                    "/project/.mise/tasks/build".into()
                ))
                .map(|value| value.0.as_str()),
            Some("mise-task")
        );
        assert!(
            !icons
                .keys()
                .any(|(_, path)| path == "/home/user/.config/mise/config.toml")
        );
        assert!(!icons.contains_key(&("mise-file-tasks".into(), "/project/mise.toml".into())));
        assert_eq!(
            file_task_name("/project/.mise/tasks/group/build.sh").as_deref(),
            Some("group:build")
        );
    }

    #[test]
    fn activation_subscribes_to_unsaved_mise_configuration_and_file_tasks() {
        let host = MockHost::install().with_capabilities(&[
            "project.read",
            "editor.read",
            "storage",
            "events",
        ]);
        let mut plugin = OpenideaMise::default();

        plugin
            .activate(&openidea_plugin::Context::default())
            .unwrap();

        let subscriptions = host.path_subscriptions();
        assert!(
            subscriptions["document-changed"]
                .iter()
                .any(|glob| glob == "**/.mise/tasks/**")
        );
        assert!(
            subscriptions["document-changed"]
                .iter()
                .any(|glob| glob == "**/mise*.toml")
        );
    }

    #[test]
    fn unsaved_document_changes_invalidate_without_running_mise() {
        let host = MockHost::install().with_capabilities(&[
            "project.read",
            "editor.read",
            "storage",
            "events",
            "process:mise",
        ]);
        host.set_project_root(Some("/project"));
        state::save(&Snapshot {
            project: "/project".into(),
            context: "/project".into(),
            tasks: vec![MiseTask {
                name: "build".into(),
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
        let mut plugin = OpenideaMise::default();
        assert_eq!(
            plugin.snapshot("/project", "/project").tasks[0].name,
            "build"
        );

        plugin.handle_events(
            &[Event::DocumentChanged(openidea_plugin::DocumentChanged {
                path: "/project/mise.toml".into(),
                base_version: 1,
                version: 2,
                changes: Vec::new(),
            })],
            &host.invocation(),
        );

        assert!(host.process_runs().is_empty());
        let message: Value = serde_json::from_str(&host.view_messages().last().unwrap().1).unwrap();
        assert_eq!(message["op"], "invalidate");
        assert_eq!(message["project"], "/project");
        assert_eq!(message["refresh"], false);
    }

    #[test]
    fn snapshot_messages_never_expose_a_redacted_secret() {
        let host = host_with_mise();
        let mut plugin = OpenideaMise::default();
        let snapshot = plugin.refresh(&host.invocation()).unwrap();
        let message = snapshot_message(&snapshot, None, None).to_string();
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
    fn web_view_messages_are_scoped_to_the_requesting_frame() {
        let host = host_with_mise();
        let mut plugin = OpenideaMise::default();
        plugin
            .handle(
                VIEW,
                ViewEvent::Message(
                    r#"{"op":"ready","sessionId":"window-project-session","requestId":7}"#.into(),
                ),
                &host.invocation(),
            )
            .unwrap();

        let messages = host.view_messages();
        let snapshot: Value = serde_json::from_str(&messages[0].1).unwrap();
        assert_eq!(snapshot["sessionId"], "window-project-session");
        assert_eq!(snapshot["requestId"], 7);
        assert_eq!(snapshot["project"], "/project");
    }

    #[test]
    fn web_view_snapshot_failures_are_returned_to_the_requesting_frame() {
        let host = MockHost::install().with_capabilities(&["project.read", "storage", "events"]);
        host.set_project_root(None);
        let mut plugin = OpenideaMise::default();

        plugin
            .handle(
                VIEW,
                ViewEvent::Message(
                    r#"{"op":"ready","sessionId":"empty-window","requestId":11}"#.into(),
                ),
                &host.invocation(),
            )
            .unwrap();

        let message: Value = serde_json::from_str(&host.view_messages().last().unwrap().1).unwrap();
        assert_eq!(message["op"], "error");
        assert_eq!(message["sessionId"], "empty-window");
        assert_eq!(message["requestId"], 11);
        assert!(
            message["message"]
                .as_str()
                .unwrap()
                .contains("open a project")
        );
    }

    #[test]
    fn untrusted_project_offers_an_explicit_trust_flow() {
        let host = MockHost::install().with_capabilities(&[
            "project.read",
            "storage",
            "events",
            "process:mise",
        ]);
        host.set_project_root(Some("/project"));
        let trusted = Rc::new(Cell::new(false));
        let process_trusted = trusted.clone();
        host.on_process("mise", move |args, _| {
            if args == ["--yes", "trust"] {
                process_trusted.set(true);
                return Ok(ProcessOutput {
                    status: Some(0),
                    stdout: "trusted".into(),
                    stderr: String::new(),
                });
            }
            if !process_trusted.get() {
                return Ok(ProcessOutput {
                    status: Some(1),
                    stdout: String::new(),
                    stderr: "Config files in /project/mise.toml are not trusted.".into(),
                });
            }
            let stdout = match args {
                [a, b, c] if a == "tasks" && b == "ls" && c == "--json" => "[]",
                [a, b] if a == "ls" && b == "--json" => "{}",
                [a, b] if a == "env" && b == "--json" => "{}",
                [a, b, c] if a == "config" && b == "ls" && c == "--json" => "[]",
                _ => return Err(Error::Invalid(format!("unexpected mise args: {args:?}"))),
            };
            Ok(ProcessOutput {
                status: Some(0),
                stdout: stdout.into(),
                stderr: String::new(),
            })
        });

        let mut plugin = OpenideaMise::default();
        plugin
            .handle(
                VIEW,
                ViewEvent::Message(r#"{"op":"ready","sessionId":"trust-session"}"#.into()),
                &host.invocation(),
            )
            .unwrap();
        let first: Value = serde_json::from_str(&host.view_messages().last().unwrap().1).unwrap();
        assert_eq!(first["trustRequired"], true);
        assert_eq!(host.process_runs().len(), 1);

        plugin
            .handle(
                VIEW,
                ViewEvent::Message(r#"{"op":"trust","sessionId":"trust-session"}"#.into()),
                &host.invocation(),
            )
            .unwrap();
        let second: Value = serde_json::from_str(&host.view_messages().last().unwrap().1).unwrap();
        assert_eq!(second["trustRequired"], false);
        assert_eq!(second["sessionId"], "trust-session");
        assert_eq!(host.process_runs()[1].args, ["--yes", "trust"]);
    }

    #[test]
    fn packaged_web_view_has_no_duplicate_mise_heading() {
        const PAGE: &str = include_str!("../web/index.html");
        const MANIFEST: &str = include_str!("../plugin.toml");
        const ICON: &str = include_str!("../icons/mise.svg");
        const SETTINGS: &str = include_str!("../settings.schema.json");

        assert!(PAGE.contains("<!doctype html>"));
        assert!(PAGE.contains("globalThis.openidea"));
        assert!(PAGE.contains("sessionId"));
        assert!(PAGE.contains("latestRequestId"));
        assert!(PAGE.contains("responseRequestId < latestRequestId"));
        assert!(PAGE.contains("if (state.loading) return"));
        assert!(PAGE.contains("::-webkit-scrollbar-track"));
        assert!(PAGE.contains("scrollbar-color: var(--openidea-border) transparent"));
        assert!(PAGE.contains(r#"requestSnapshot("trust", true)"#));
        assert!(PAGE.contains("Mise configuration has unsaved changes"));
        assert!(PAGE.contains("Cancel"));
        assert!(PAGE.contains(r#"id="refresh""#));
        assert!(PAGE.contains("toolSourceInfo"));
        assert!(PAGE.contains(r#"make("div", "tool-groups")"#));
        assert!(PAGE.contains("Nested project"));
        assert!(PAGE.contains("Installed only"));
        assert!(PAGE.contains("Installed by Mise but not selected by the current configuration"));
        assert!(!PAGE.contains("<strong>Mise</strong>"));
        assert!(MANIFEST.contains(r#"web = "web/index.html""#));
        assert!(MANIFEST.contains(r#"header = "none""#));
        assert!(ICON.contains(r##"fill="#CACCD2""##));
        assert!(ICON.contains(r##"stroke="#CACCD2""##));
        assert!(!ICON.contains("data-themed"));
        let settings: Value = serde_json::from_str(SETTINGS).unwrap();
        assert_eq!(
            settings["properties"]["registerRunTarget"]["default"],
            false
        );
        assert_eq!(
            settings["properties"]["followEditorContext"]["default"],
            true
        );
    }

    #[test]
    fn config_names_are_detected_on_unix_and_windows() {
        assert!(is_config_path("/project/.mise.toml"));
        assert!(is_config_path(r"C:\project\mise.local.toml"));
        assert!(is_config_path("/project/mise.dev.toml"));
        assert!(is_config_path("/project/.mise.dev.local.toml"));
        assert!(is_config_path("/project/.config/mise/config.toml"));
        assert!(is_config_path("/project/.mise/conf.d/node.toml"));
        assert!(!is_config_path("/project/Cargo.toml"));
    }

    #[test]
    fn active_editor_directory_drives_queries_and_nested_task_runs() {
        let host = MockHost::install().with_capabilities(&[
            "project.read",
            "storage",
            "events",
            "process:mise",
        ]);
        host.set_project_root(Some("/project"));
        host.open_editor("/project/backend/src/main.rs", "rust", "fn main() {}", &[]);
        host.on_process("mise", |args, _| {
            let stdout = match args {
                [flag, context, a, b, c]
                    if flag == "-C"
                        && context == "/project/backend/src"
                        && a == "tasks"
                        && b == "ls"
                        && c == "--json" =>
                {
                    r#"[{"name":"build","source":"/project/backend/mise.toml"}]"#
                }
                [flag, context, a, b]
                    if flag == "-C"
                        && context == "/project/backend/src"
                        && a == "ls"
                        && b == "--json" =>
                {
                    "{}"
                }
                [flag, context, a, b]
                    if flag == "-C"
                        && context == "/project/backend/src"
                        && a == "env"
                        && b == "--json" =>
                {
                    "{}"
                }
                [flag, context, a, b, c]
                    if flag == "-C"
                        && context == "/project/backend/src"
                        && a == "config"
                        && b == "ls"
                        && c == "--json" =>
                {
                    r#"[{"path":"/project/backend/mise.toml"}]"#
                }
                _ => return Err(Error::Invalid(format!("unexpected mise args: {args:?}"))),
            };
            Ok(ProcessOutput {
                status: Some(0),
                stdout: stdout.into(),
                stderr: String::new(),
            })
        });

        let mut plugin = OpenideaMise::default();
        let snapshot = plugin.refresh(&host.invocation()).unwrap();
        assert_eq!(snapshot.context, "/project/backend/src");
        assert_eq!(snapshot.tasks[0].name, "build");
        assert_eq!(host.process_runs().len(), 4);
        assert!(host.process_runs().iter().all(|run| {
            run.args
                .starts_with(&["-C".into(), "/project/backend/src".into()])
        }));

        plugin.run_task(&host.invocation(), Some("build")).unwrap();
        assert_eq!(
            host.terminals()[0].args,
            ["-C", "/project/backend/src", "run", "build"]
        );
        assert!(host.configuration_runs().is_empty());
    }

    #[test]
    fn editor_context_following_can_be_disabled() {
        let host = host_with_mise();
        host.open_editor("/project/backend/src/main.rs", "rust", "fn main() {}", &[]);
        host.set_setting("followEditorContext", "false");

        let snapshot = OpenideaMise::default().refresh(&host.invocation()).unwrap();

        assert_eq!(snapshot.context, "/project");
        assert_eq!(host.process_runs()[0].args, ["tasks", "ls", "--json"]);
    }

    #[test]
    fn editor_context_is_limited_to_the_project_and_cross_platform_paths() {
        assert_eq!(
            context_directory("/project", "/project/backend/src/main.rs"),
            "/project/backend/src"
        );
        assert_eq!(
            context_directory("/project", "/other/backend/main.rs"),
            "/project"
        );
        assert_eq!(
            context_directory(r"C:\Project", r"c:\project\backend\main.rs"),
            "c:/project/backend"
        );
        assert!(same_path(r"C:\Project", r"c:/project/"));
    }

    #[test]
    fn denied_process_capability_degrades_to_section_errors() {
        let host = MockHost::install().with_capabilities(&["project.read", "storage", "events"]);
        host.set_project_root(Some("/project"));
        let snapshot = OpenideaMise::default().refresh(&host.invocation()).unwrap();
        assert_eq!(snapshot.errors.len(), 4);
        assert!(
            snapshot
                .errors
                .iter()
                .all(|error| error.contains("permission to run mise"))
        );
    }
}
