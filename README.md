# OpenIDEA Mise

Mise integration for OpenIDEA. The plugin provides a project task center,
installed-tool and environment inspection, dynamic run configurations, Mise TOML
navigation and a `mise exec` run target.

The port is implemented against OpenIDEA Plugin API 1.8 and runs as a sandboxed
`wasm32-wasip2` component.

## Features

- Browse, search and run `mise` tasks from the Mise tool window.
- Inspect installed tools and the environment reported by the current project.
  Tools are grouped by the configuration that selected them, with distinct
  nested-project, project, inherited, global, system, and unselected-installed
  sections.
- Follow Mise's parent, global, local, environment-specific, and nested
  configuration hierarchy from the active editor directory. The Configuration
  tab shows the files reported by `mise config ls --json` and the effective
  source of tasks and tools.
- Visualize task dependencies, post-dependencies, wait-for edges, missing tasks,
  and cycles.
- Complete and navigate task names in Mise TOML files, with task hover,
  Rename/Find Usages indexing, Run gutter actions, Shell script injection, and
  a quick fix that converts TOML 1.1 multiline task environments to dotted
  keys understood by TOML 1.0-only language servers.
- Show Mise-aware Structure entries, live diagnostics, and semantic highlighting
  while editing unsaved TOML documents.
- Recognize Mise File Tasks under `mise-tasks`, `.mise-tasks`, `mise/tasks`,
  `.mise/tasks`, and `.config/mise/tasks`, including `#MISE` metadata completion,
  dependency navigation, diagnostics, highlighting, and Run gutter actions.
- Apply the official Mise icon dynamically to recognized configuration files,
  File Task folders, and File Task sources in the project tree.
- Use generated run configurations named exactly after each task. The optional
  `mise.exec` Run Target is disabled by default so task labels stay compact; its
  pre-launch validation rejects untrusted or out-of-project working directories.

Environment variables whose names look sensitive are masked by default. A
revealed value remains in the current tool-window session and is never written
to plugin storage.

## Capabilities

- `project.read` lets the plugin associate Mise configuration files with the
  open project and publish editor/indexing features.
- `editor.read` lets document-change events invalidate Mise state without
  forcing the user to save before live editor providers are recomputed.
- `storage` caches task and tool metadata so editor features do not invoke a
  process. Environment values are deliberately excluded.
- `events` invalidates that cache when Mise files change and reacts to plugin
  settings changes.
- Optional `process:mise` runs the locally installed `mise` executable for
  refresh, tasks, generated run configurations, and the Mise Run Target. If
  this high-risk capability is denied, TOML editing features still work and the
  tool window reports that refresh is unavailable.

The `mise.exec` Run Target can be enabled in the plugin settings when other run
configurations should be wrapped with `mise exec --`. It is opt-in because
selecting it as OpenIDEA's project-wide default appends a target label to every
run configuration. OpenIDEA Plugin API 1.8 supports Run and Test through this
target, but rejects Debug on plugin-provided targets.

The tool window follows the focused editor directory by default. Disable
**Follow the active editor directory** in the plugin settings to resolve all
Mise data from the project root instead. Generated Run Configurations use the
project-root context because OpenIDEA's configuration provider is project
scoped; a task selected from a nested context is opened in a terminal with
`mise -C <directory> run <task>`.

When Mise reports an untrusted configuration, the tool window shows explicit
**Trust** and **Cancel** actions. Trust invokes `mise --yes trust` in the active
context, then refreshes the view.

## Development

Requirements:

- Rust 1.98.1 with `wasm32-wasip2`
- `mise`
- the `openidea-plugin` CLI from OpenIDEA v0.1.45

Common commands:

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
openidea-plugin build .
openidea-plugin check .
```

With mise, the complete local workflow is:

```sh
mise run check
mise run package-local
```

`package-local` builds and validates an unsigned
`dist/winlans.mise-<version>-local.oiplugin` that can be installed from disk.
It does not read or use the production publisher signing key. The signed
release path remains `scripts/build-package.sh`. Verify the local artifact with
`sha256sum -c dist/winlans.mise-<version>-local.oiplugin.sha256`.

The project-level `mise.toml` pins Rust and exposes the complete build,
validation, unsigned local packaging, and signed release packaging workflows.
If Mise blocks the file on first use, review it and run `mise trust mise.toml`
before invoking these tasks.

## Troubleshooting

- If tasks change when moving between folders, this is Mise's normal hierarchy
  behavior. The footer shows the active directory context and the Configuration
  tab shows the loaded chain.
- If the Run/Debug selector does not show Mise tasks, ensure a supported
  root-level marker such as `mise.toml`, `.mise.toml`, `.tool-versions`, or
  `.miserc.toml` exists, the project is trusted, and the optional
  `process:mise` capability is granted. Then refresh the project or reopen it.
- The one-time “Use Mise” Run Target prompt is remembered by OpenIDEA per
  project/plugin/target. Re-enabling the setting does not reset that answer;
  choose Mise explicitly under Settings › Run Targets or a configuration's
  **Run on** selector.
- A selected plugin Run Target cannot be used for Debug in the current
  OpenIDEA Plugin API. Use Local machine for Debug.
- Mise accepts TOML 1.1 multiline inline tables, but Taplo/Better TOML may
  still diagnose them with messages such as `expected new line`. Use
  **Alt+Enter → Mise: Convert multiline env to TOML 1.0-compatible keys**;
  task `env` completion also emits the compatible dotted-key form.
- Shell language injection for task `run`/`script` values is isolated from
  Mise's TOML providers, so injected commands are never parsed again as a
  complete TOML document.

See [the port plan](docs/openidea-mise-port-plan.md) for the staged feature and
release roadmap. See [the release runbook](docs/releasing.md) for signing-key,
GitHub Environment, and automatic Marketplace publication setup.

## Security

The plugin does not request network access. It runs only the approved `mise`
program in the open project, passes arguments as argv without shell
concatenation, and does not execute a process from completion, hover,
navigation, indexing, or injection providers. Sensitive environment values are
never written to plugin storage or logs.

## License

Apache-2.0. This project is an independent OpenIDEA port inspired by
`134130/intellij-mise`, also licensed under Apache-2.0.
