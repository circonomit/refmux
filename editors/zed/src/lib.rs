use zed_extension_api::{
    self as zed, LanguageServerId, Result, serde_json::Value, settings::LspSettings,
};

const PROXY: &str = "refmux";

struct RefmuxExtension;

/// Options come from `lsp.refmux.settings`: Zed replaces the whole argument list with
/// `binary.arguments` and skips the extension entirely when `binary.path` is set.
fn proxy_args(settings: Option<&Value>) -> Vec<String> {
    let mut args = Vec::new();
    let Some(settings) = settings else {
        return args;
    };
    if let Some(seconds) = settings.get("idle_timeout").and_then(Value::as_u64) {
        args.extend(["--idle-timeout".to_owned(), seconds.to_string()]);
    }
    if settings.get("formatting").and_then(Value::as_bool) == Some(true) {
        args.push("--formatting".to_owned());
    }
    if settings.get("env_placeholders").and_then(Value::as_bool) == Some(false) {
        args.push("--no-env-placeholders".to_owned());
    }
    args
}

/// `dbt` resolves on the worktree's shell PATH, where mise or a venv decide which dbt that is.
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

impl zed::Extension for RefmuxExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let command = worktree.which(PROXY).ok_or_else(|| {
            format!(
                "{PROXY} not found on PATH; install it with `cargo install --locked --git https://github.com/circonomit/refmux`"
            )
        })?;
        let settings = LspSettings::for_worktree(server_id.as_ref(), worktree)?.settings;
        let mut args = proxy_args(settings.as_ref());
        args.push("--".to_owned());
        args.extend(dbt_command(settings.as_ref()));
        Ok(zed::Command {
            command,
            args,
            env: worktree.shell_env(),
        })
    }
}

zed::register_extension!(RefmuxExtension);

#[cfg(test)]
mod tests {
    use super::*;
    use zed::serde_json::json;

    #[test]
    fn passes_no_proxy_options_by_default() {
        assert!(proxy_args(None).is_empty());
        let defaults = json!({ "formatting": false, "env_placeholders": true });
        assert!(proxy_args(Some(&defaults)).is_empty());
    }

    #[test]
    fn maps_settings_to_proxy_options() {
        let settings = json!({ "idle_timeout": 0, "formatting": true, "env_placeholders": false });
        assert_eq!(
            proxy_args(Some(&settings)),
            [
                "--idle-timeout",
                "0",
                "--formatting",
                "--no-env-placeholders"
            ]
        );
    }

    #[test]
    fn defaults_to_dbt_lsp_without_telemetry() {
        let expected = ["dbt", "lsp", "--no-send-anonymous-usage-stats"];
        assert_eq!(dbt_command(None), expected);
        assert_eq!(dbt_command(Some(&json!({ "dbt_command": [] }))), expected);
    }

    #[test]
    fn replaces_the_dbt_command() {
        let settings = json!({ "dbt_command": ["uv", "run", "dbt", "lsp"] });
        assert_eq!(dbt_command(Some(&settings)), ["uv", "run", "dbt", "lsp"]);
    }
}
