# OpenIDEA Mise

Mise integration for OpenIDEA. The plugin provides a project task center,
installed-tool and environment inspection, dynamic run configurations, Mise TOML
navigation and a `mise exec` run target.

The port is implemented against OpenIDEA Plugin API 1.4 and runs as a sandboxed
`wasm32-wasip2` component.

## Features

- Browse, search and run `mise` tasks from the Mise tool window.
- Inspect installed tools and the environment reported by the current project.
- Visualize task dependencies, post-dependencies, wait-for edges, missing tasks,
  and cycles.
- Complete and navigate task names in Mise TOML files, with task hover,
  Rename/Find Usages indexing, Run gutter actions, and Shell script injection.
- Use generated `Mise: <task>` run configurations or the optional `mise.exec`
  Run Target.

Environment variables whose names look sensitive are masked by default. A
revealed value remains in the current tool-window session and is never written
to plugin storage.

## Capabilities

- `project.read` lets the plugin associate Mise configuration files with the
  open project and publish editor/indexing features.
- `storage` caches task and tool metadata so editor features do not invoke a
  process. Environment values are deliberately excluded.
- `events` invalidates that cache when Mise files or settings change.
- Optional `process:mise` runs the locally installed `mise` executable for
  refresh, tasks, generated run configurations, and the Mise Run Target. If
  this high-risk capability is denied, TOML editing features still work and the
  tool window reports that refresh is unavailable.

## Development

Requirements:

- Rust 1.98.1 with `wasm32-wasip2`
- `mise`
- the `openidea-plugin` CLI from OpenIDEA v0.1.40

Common commands:

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
openidea-plugin build .
openidea-plugin check .
```

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
