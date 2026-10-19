# Refmux for dbt

refmux runs the dbt™ Fusion language server (`dbt lsp`) for every dbt project in a repository. Each document goes to the server of its nearest `dbt_project.yml`, so a monorepo with several dbt projects works without configuration in editors that start one language server per workspace, such as Zed.

Not affiliated with dbt Labs. Needs dbt Fusion, the `dbt` binary with the `lsp` subcommand; dbt-core and dbt-oss ship no language server.

Status: 0.1. The proxy is tested over stdio against five dbt projects in one repository. The Zed extension runs in Zed 1.24 on macOS: hover with parent and child models, one server per project. Its `lsp.refmux.settings` options are covered by unit tests.

## Why a proxy

`dbt lsp` serves one project per process. It needs `--project-dir`, ignores further workspace folders and does not advertise workspace-folder support (verified on 2.0.8). dbt's VS Code extension therefore runs one server per project and picks one by the active file's project root. Zed runs one server per worktree and reports subprojects as workspace folders, which `dbt lsp` ignores.

This proxy does on the server side what the VS Code extension does on the client side. It becomes unnecessary once `dbt lsp` serves several projects itself, which [dbt-labs/dbt#16801](https://github.com/dbt-labs/dbt/issues/16801) asks for.

## How it works

- **Routing**: a document belongs to the nearest `dbt_project.yml` above it. Installed packages (`dbt_packages/`, `dbt_modules/` or the project's `packages-install-path`) belong to the enclosing project.
- **Lifecycle**: a project's server starts with the first document opened in it and stops once it has had no open documents for `--idle-timeout` (default 10 minutes). One server took 100–380 MB on the projects we measured.
- **Other files**: SQL and YAML outside any dbt project get empty answers, never errors.
- **Isolation**: request ids, progress tokens and capability registrations are namespaced per project (every `dbt lsp` registers its file watcher as `default-dbt-file-system-watcher`). A stopped server's diagnostics and watchers are withdrawn.
- **Environment**: each server runs in its project directory, so dbt loads that project's `.env`. An `env_var('X')` without default in `profiles.yml` or `dbt_project.yml` that is not set, not in `.env` and not given a default by another call (such as the active target's) gets a placeholder: `1` or `false` where dbt needs a number or a boolean (`threads`, `| as_number`, `| as_bool`), otherwise a path in a per-user directory (the temp directory on macOS and Windows, `$XDG_CACHE_HOME/refmux` or `~/.cache/refmux` elsewhere): dbt cannot render the profile without it, and the server then stays silent. Static analysis opens the warehouse only for hooks such as `on-run-start`, which `dbt lsp` runs on every compile, so with such hooks a DuckDB profile gets a scratch database at that path. Set the real value to use the real warehouse, for example for `dbt.show`.
- **Capabilities** are pinned to what `dbt lsp` 2.0.8 advertises, since the client needs them before any server runs. A server that advertises different ones is logged.
- **Formatting** is not advertised unless `--formatting` is given. Zed formats with the first server that offers it, so dbt's formatter would otherwise replace sqruff, sqlfluff or prettier.

## Install

Needs Rust 1.88 or newer (the `rust-version` in `Cargo.toml`); run `rustup update` if cargo reports an older rustc.

```sh
cargo install --locked --git https://github.com/circonomit/refmux
```

From a checkout: `cargo install --locked --path .`

## Usage

```
refmux [OPTIONS] [-- <server command>...]

The server command defaults to `dbt lsp`; --project-dir is appended, and --profiles-dir
when the project has a profiles.yml and neither the command nor DBT_PROFILES_DIR (also
from the project's .env) names another.

Options:
  --idle-timeout <seconds>  Stop a project's server after it has had no open documents
                            for this long (default 600, 0 keeps servers running)
  --formatting              Advertise dbt's document formatting
  --no-env-placeholders     Do not fill unset env_var() references with placeholder paths
```

Any LSP client can start it as a stdio server. For Zed, see [`editors/zed`](editors/zed/README.md).

## Known limitations

- `dbt lsp` answers nothing for documents whose path differs from its canonical project path, for example a checkout reached through a symlink (macOS `/tmp` is one). Reported in [dbt-labs/dbt#16800](https://github.com/dbt-labs/dbt/issues/16800).
- Fusion's parser flags some DuckDB syntax (`struct_pack(*columns)`, `unnest … with ordinality`) as `dbt0101` warnings. `--static-analysis off` in the server command silences them; hover, definition and completion keep working.
- `dbt lsp` 2.0.8 analyses files as saved: after an edit, hover in that file stays empty until the file is saved. It offers no hover on model entries in schema YAML.
- A server that crashes restarts with the next document opened in its project. Documents that were already open stay without answers until reopened or until the language server is restarted.

## License

Apache-2.0

---

dbt is a trademark of dbt Labs, LLC. This project is not affiliated with or endorsed by dbt Labs.
