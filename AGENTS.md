# OpenIDEA Mise — AI Project Context

## Project

- This is an independent Rust port of Mise integration for OpenIDEA (`winlans.mise`).
- Current line: `0.2.0`; OpenIDEA Plugin API `1.10`; CLI/SDK `v0.1.47`; runtime target `wasm32-wasip2`; Rust edition 2024, minimum Rust 1.98.
- User-facing behavior is in `README.md`; release operations in `docs/releasing.md`; `CHANGELOG.md` is historical and intentionally brief.

## Architecture

- `src/lib.rs`: plugin entry point, host lifecycle, tool-window message bridge,
  snapshots, Run/Launch Configurations, optional `mise.exec` Run Target, file icons.
- `src/mise.rs`: the only Mise process client (`tasks ls`, `ls`, `env`, `config ls`,
  and trust); always passes argv and uses `mise -C <context>` when needed.
- `src/model.rs`: bounded JSON parsing, snapshot/view serialization, redaction flags,
  stable task IDs, and task debugger inference.
- `src/state.rs`: project/context-keyed persistent cache; task/tool/config metadata only.
  Environment values are never cached.
- `src/graph.rs`: dependency, `depends_post`, `wait_for`, missing-task, and cycle graph.
- `src/toml_support.rs`: TOML and Mise File Task completion, navigation, hover,
  diagnostics, semantic tokens, indexing/rename, injections, and Run gutter actions.
- `web/index.html`: static Mise tool window; it communicates with Wasm through the
  OpenIDEA message bridge and exposes Run, not a generic Debug button.
- `plugin.toml`, `settings.schema.json`, `icons/`: manifest, capabilities/contributions,
  settings, and the official Mise icon; `scripts/` owns version checks and packaging.

## Runtime and safety contracts

- Required capabilities: `project.read`, `editor.read`, `storage`, `events`;
  `process:mise` is optional. Without it, editing works but refresh/run must report
  that Mise execution is unavailable.
- The active editor directory is the default Mise context; `followEditorContext=false`
  forces the project root. Generated Run/Launch Configurations use the project root.
- Keep API 1.4 `RunConfigurations` alongside API 1.9+ `LaunchConfigurations`.
  Automatic debugger metadata must be conservative; use `debugTaskRules` for wrappers.
- Never run Mise from completion, hover, navigation, indexing, or injection providers.
  Do not concatenate shell commands; do not add network access or persist secrets.
- Sensitive environment values are redacted by default and excluded from storage/logs;
  cache limits and parser bounds in `src/model.rs`/`src/state.rs` are intentional.

## Development and release

```sh
mise run check          # format, Clippy, Rust tests, script tests, build, plugin check
mise run package-local  # unsigned installable package in dist/
```

- Keep `Cargo.toml`, `Cargo.lock`, `plugin.toml`, and release metadata versions aligned;
  use `python3 scripts/check_versions.py` before packaging.
- Signed release packaging requires `OPENIDEA_SIGNING_KEY_FILE`; never commit or log
  publisher keys; `scripts/update_registry.py` must remain idempotent.

## Agent working rules

- Preserve unrelated working-tree changes; inspect `git status` before editing.
- Prefer small targeted changes and tests. Update fixtures/tests when changing JSON
  parsing, host callbacks, settings, or OpenIDEA API integration.
- Keep `CHANGELOG.md` concise; do not create a parallel `.agents/changelog`.
- For release work, follow `docs/releasing.md`, verify the generated package, and do not
  claim it is published/live without checking the relevant CI/release state.
