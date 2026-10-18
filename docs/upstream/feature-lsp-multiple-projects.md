# [Feature] `dbt lsp`: serve several dbt projects from one language server

Filed 2026-10-18 as https://github.com/dbt-labs/dbt/issues/16801 (text below as filed). Duplicate search the same day found nothing; related VS Code extension issues: #13721, #13758.

---

**Title:** [Feature] `dbt lsp`: serve several dbt projects from one language server

### Is this your first time submitting a feature request?

- [x] I have read the expectations for open source contributors
- [x] I have searched the existing issues, and I could not find an existing issue for this feature
- [x] I am requesting a straightforward extension of existing dbt functionality, rather than a Big Idea better suited to a discussion

### Which version of dbt is this feature for?

dbt v2.x (Core or Fusion)

### Describe the feature

`dbt lsp` serves exactly one dbt project per process. Verified with dbt 2.0.8:

- Started from a repository root without `--project-dir`, the server answers `initialize`, but returns `null` for hover, definition and completion and publishes no diagnostics. This also happens when `initialize` lists every dbt project of the repository as a workspace folder.
- The `initialize` result advertises no `workspace.workspaceFolders` capability, so a client cannot add projects through `workspace/didChangeWorkspaceFolders`.

The VS Code extension works around this on the client side: it runs one language server per dbt project and picks one by the active file's project root (described in #13721). Other LSP clients cannot do that. Zed, for example, runs one server per worktree and reports subprojects as workspace folders, so in a repository with several dbt projects none of them gets language features unless the worktree root happens to be a dbt project.

Proposal: one `dbt lsp` process serves every dbt project below its workspace folders and routes each document to its nearest `dbt_project.yml`, skipping installed packages. This is how `tsserver` maps files to their nearest `tsconfig.json` and how `gopls` (since v0.15) keeps one build per module in a single process.

1. Advertise `workspace.workspaceFolders` (`supported: true`, `changeNotifications: true`) and handle `workspace/didChangeWorkspaceFolders`.
2. Load projects lazily: on `textDocument/didOpen`, find the nearest `dbt_project.yml` above the document and load that project unless it is loaded already.
3. Keep `--project-dir` as an explicit single-project mode.

### Describe alternatives you've considered

- One editor window per dbt project. This works, but loses the repository around the projects.
- A proxy that starts one `dbt lsp` per project and routes messages by document URI. We built one for Zed ([refmux](https://github.com/circonomit/refmux)). It works, but it has to pin the server's capabilities and rewrite request ids, progress tokens and capability registrations: every server registers its file watcher as `default-dbt-file-system-watcher`, so the registrations collide at the client. It also pays the full memory cost per project, 100 to 380 MB per server in our repository. Native support would make the proxy unnecessary.

### Who will this benefit?

Teams with more than one dbt project in a repository (monorepos, several local projects in a mesh setup, consultancies with one project per client) who use an editor other than VS Code: Zed, Neovim, Helix, or any other LSP client. The VS Code extension could also drop its per-project process management.

### Are you interested in contributing this feature?

dbt-oss 2.0.8 has no `lsp` subcommand, so the server appears to live in the proprietary distribution and we cannot send code. We are glad to test pre-releases and to share the proxy's routing logic.

### Anything else?

Environment: dbt 2.0.8 (Fusion distribution), DuckDB adapter, macOS 27.0.1 on arm64, five dbt projects in one repository.

Related: #16800 (`dbt lsp` stays silent when it cannot load the project).
