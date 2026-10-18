# [v2 Bug] `dbt lsp` stays silent when it cannot load the project

Filed 2026-10-18 as https://github.com/dbt-labs/dbt/issues/16800 (text below as filed). Duplicate search the same day found nothing.

---

**Title:** [v2 Bug] `dbt lsp` stays silent when it cannot load the project

### Is this a new bug in dbt v2.x compared to the latest version of dbt 1.x?

- [x] I believe this is a new bug in dbt v2.x
- [x] I have searched the existing issues and could not find a duplicate

### Current Behavior

In both cases below, `dbt lsp` answers `initialize` and logs `Did open: file:///…` for each opened document. After that it returns `null` for every request and publishes no diagnostics. Nothing tells the user what is wrong.

1. **The profile cannot be rendered.** `profiles.yml` uses `env_var('X')` without a default and `X` is not set. `dbt parse` reports `[InvalidConfig (dbt1005)]: Jinja render error: invalid operation: 'env_var': environment variable 'X' not found`. The language server reports nothing.
2. **The project path contains a symlink.** On macOS, `/tmp` links to `/private/tmp`. With `--project-dir /tmp/proj` and documents at `file:///tmp/proj/…`, every request returns `null`. The same project under `/private/tmp/proj` works.

### Expected Behavior

1. The load error reaches the client: a `window/showMessage` and a diagnostic on `profiles.yml`, with the text `dbt parse` prints.
2. Documents match the project regardless of symlinks, for example by comparing canonical paths on both sides. If they don't match, the server says so.

### Steps To Reproduce

1. Create a project:
   - `dbt_project.yml`: `name: envtest`, `version: '1.0'`, `profile: envtest`, `model-paths: ['models']`
   - `profiles.yml`: profile `envtest`, target `local`, `type: duckdb`, `path: "{{ env_var('ENVTEST_DUCKDB_PATH') }}"`
   - `models/m1.sql`: `select 1 as a`
   - `models/m2.sql`: `select * from {{ ref('m1') }}`
2. In the project directory, with `ENVTEST_DUCKDB_PATH` unset, start `dbt lsp --project-dir <dir> --profiles-dir <dir>`. Send `initialize` and `initialized`, open `models/m2.sql` and request hover on `m1`. The result is `null`. The only messages are `Did open` log lines, and no diagnostics arrive.
3. Set `ENVTEST_DUCKDB_PATH` (or put it in `<dir>/.env`) and repeat. Hover returns `Children Models: m2`.
4. With the variable set, place the project under `/tmp/…` on macOS and use `/tmp/…` paths for `--project-dir` and the document URIs. Hover returns `null`. With `/private/tmp/…` paths it works.

### Relevant log output

```
window/logMessage: Did open: file:///tmp/dbt-env-test/proj/models/m2.sql
```

Nothing else, in both cases.

### Environment

- OS: macOS 27.0.1 (arm64)
- dbt: 2.0.8 (Fusion distribution; `dbt --version` prints `dbt 2.0.8`)
- Client: a plain stdio LSP client. We did not check whether the VS Code extension surfaces these cases through its own UI.

### Which database adapter are you using?

DuckDB

### Is this a discrepancy vs. dbt 1.x?

No, dbt 1.x has no language server.

### Additional Context

Both cases share the symptom. Happy to split them into two issues if that is easier to track.
