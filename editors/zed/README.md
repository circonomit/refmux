# Refmux for dbt: Zed extension

Starts [`refmux`](../../README.md) for SQL and YAML files, so every dbt project in the worktree gets its own dbt Fusion language server.

## Install

1. Install the proxy so it is on your shell `PATH`: `cargo install --locked --git https://github.com/circonomit/refmux`
2. Zed: command palette → `zed: install dev extension` → pick this folder. Zed compiles it with your rustup toolchain (`wasm32-wasip2`).
3. Open any `.sql` or `.yml` file inside a dbt project.

## Settings

```jsonc
"lsp": {
	"refmux": {
		// path of the proxy if it is not on PATH; arguments are proxy options
		"binary": { "path": "/path/to/refmux", "arguments": ["--idle-timeout", "1800"] },
		// replaces the default server command `dbt lsp --no-send-anonymous-usage-stats`
		"settings": { "dbt_command": ["dbt", "lsp", "--no-send-anonymous-usage-stats", "--static-analysis", "off"] },
	},
},
```

`dbt` resolves on the worktree's shell `PATH`, the same one your terminal uses there (mise, venvs). If that resolves to dbt-core or dbt-oss, the proxy reports that the server has no `lsp` subcommand.

To format dbt files with dbt's formatter, add `"--formatting"` to `binary.arguments` and put the server first for SQL, since Zed formats with the first server that offers it:

```jsonc
"languages": { "SQL": { "language_servers": ["refmux", "..."] } },
```

## Logs

`dev: open language server logs` → `Refmux for dbt` shows which projects started, placeholder variables and startup errors.
