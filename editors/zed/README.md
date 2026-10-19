# Refmux for dbt: Zed extension

Starts [`refmux`](../../README.md) for SQL and YAML files, so every dbt project in the worktree gets its own dbt Fusion language server.

## Install

1. Install the proxy so it is on your shell `PATH` (needs Rust 1.88 or newer): `cargo install --locked --git https://github.com/circonomit/refmux`
2. Zed: command palette → `zed: install dev extension` → pick this folder. Zed compiles it with the Rust toolchain pinned in the repository's `rust-toolchain.toml`, including `wasm32-wasip2`. rustup installs that toolchain on the first build; if rustup's auto-install is off, run `rustup toolchain install` in the checkout first.
3. Open any `.sql` or `.yml` file inside a dbt project.

## Settings

```jsonc
"lsp": {
	"refmux": {
		"settings": {
			"idle_timeout": 1800, // seconds without open documents before a project's server stops; 0 keeps it (default 600)
			"formatting": false, // true advertises dbt's formatter
			"env_placeholders": true, // false leaves unset env_var() references unset
			// replaces the default server command `dbt lsp --no-send-anonymous-usage-stats`
			"dbt_command": ["dbt", "lsp", "--no-send-anonymous-usage-stats", "--static-analysis", "off"],
		},
	},
},
```

Use `settings`, not `binary.path` or `binary.arguments`: with either one set, Zed ignores everything under `settings` above. It replaces the extension's whole argument list with `binary.arguments`, and with `binary.path` it does not ask the extension at all, so `binary.arguments` must then hold the proxy options, `--` and the full server command, for example `["--idle-timeout", "1800", "--", "dbt", "lsp", "--no-send-anonymous-usage-stats"]`. `binary.env` is fine: Zed adds it to the extension's environment and keeps the extension's arguments.

`dbt` resolves on the worktree's shell `PATH`, the same one your terminal uses there (mise, venvs). If that resolves to dbt-core or dbt-oss, the proxy reports that the server has no `lsp` subcommand.

A `language_servers` list without `"..."` starts only the servers it names. If your project settings pin SQL (or YAML) to specific servers, add `"refmux"`:

```jsonc
"languages": { "SQL": { "language_servers": ["refmux", "sqruff"] } },
```

To format dbt files with dbt's formatter, set `"formatting": true` and put `"refmux"` first for SQL, since Zed formats with the first server that offers it unless `formatter` names one.

## Logs

`dev: open language server logs` → `Refmux for dbt` shows which projects started, placeholder variables and startup errors.
