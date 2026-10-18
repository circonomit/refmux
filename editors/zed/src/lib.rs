use zed_extension_api::{
    self as zed, LanguageServerId, Result, serde_json::Value, settings::LspSettings,
};

const MUX: &str = "refmux";

struct DbtFusionLspExtension;

/// `settings.dbt_command` replaces the default server command; `dbt` resolves on the worktree's
/// shell PATH, where mise or a venv decide which dbt that is.
fn dbt_command(settings: Option<&Value>) -> Vec<String> {
    settings
        .and_then(|settings| settings.get("dbt_command"))
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .filter(|command| !command.is_empty())
        .unwrap_or_else(|| {
            ["dbt", "lsp", "--no-send-anonymous-usage-stats"]
                .map(str::to_owned)
                .to_vec()
        })
}

impl zed::Extension for DbtFusionLspExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let lsp = LspSettings::for_worktree(server_id.as_ref(), worktree)?;
        let (path, arguments, extra_env) = lsp
            .binary
            .map(|binary| (binary.path, binary.arguments, binary.env))
            .unwrap_or_default();
        let command = path.or_else(|| worktree.which(MUX)).ok_or_else(|| {
            format!("{MUX} not found on PATH; install it or set lsp.{MUX}.binary.path")
        })?;

        let mut args = arguments.unwrap_or_default();
        args.push("--".to_owned());
        args.extend(dbt_command(lsp.settings.as_ref()));

        let mut env = worktree.shell_env();
        env.extend(extra_env.unwrap_or_default());
        Ok(zed::Command { command, args, env })
    }
}

zed::register_extension!(DbtFusionLspExtension);
