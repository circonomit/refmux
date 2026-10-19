use zed_extension_api::{
    self as zed, LanguageServerId, Result, serde_json::Value, settings::LspSettings,
};

const PROXY: &str = "refmux";

struct RefmuxExtension;

/// An absent or `null` key keeps the default; a value of the wrong type is reported, not ignored.
fn setting<'a, T>(
    settings: Option<&'a Value>,
    key: &str,
    expected: &str,
    get: impl FnOnce(&'a Value) -> Option<T>,
) -> Result<Option<T>> {
    match settings.and_then(|settings| settings.get(key)) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => get(value)
            .map(Some)
            .ok_or_else(|| format!("lsp.refmux.settings.{key} must be {expected}, not {value}")),
    }
}

/// Options come from `lsp.refmux.settings`: Zed replaces the whole argument list with
/// `binary.arguments` and skips the extension entirely when `binary.path` is set.
fn proxy_args(settings: Option<&Value>) -> Result<Vec<String>> {
    let mut args = Vec::new();
    if let Some(seconds) = setting(
        settings,
        "idle_timeout",
        "a non-negative whole number of seconds",
        Value::as_u64,
    )? {
        args.extend(["--idle-timeout".to_owned(), seconds.to_string()]);
    }
    if setting(settings, "formatting", "true or false", Value::as_bool)? == Some(true) {
        args.push("--formatting".to_owned());
    }
    if setting(
        settings,
        "env_placeholders",
        "true or false",
        Value::as_bool,
    )? == Some(false)
    {
        args.push("--no-env-placeholders".to_owned());
    }
    Ok(args)
}

/// `dbt` resolves on the worktree's shell PATH, where mise or a venv decide which dbt that is.
fn dbt_command(settings: Option<&Value>) -> Result<Vec<String>> {
    let command = setting(settings, "dbt_command", "an array of strings", |value| {
        value
            .as_array()?
            .iter()
            .map(|part| part.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
    })?;
    Ok(command
        .filter(|command| !command.is_empty())
        .unwrap_or_else(|| {
            ["dbt", "lsp", "--no-send-anonymous-usage-stats"]
                .map(str::to_owned)
                .to_vec()
        }))
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
                "{PROXY} not found on PATH; install it with Rust 1.88 or newer: `cargo install --locked --git https://github.com/circonomit/refmux`"
            )
        })?;
        let settings = LspSettings::for_worktree(server_id.as_ref(), worktree)?.settings;
        let mut args = proxy_args(settings.as_ref())?;
        args.push("--".to_owned());
        args.extend(dbt_command(settings.as_ref())?);
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
        assert!(proxy_args(None).unwrap().is_empty());
        let defaults = json!({ "formatting": false, "env_placeholders": true });
        assert!(proxy_args(Some(&defaults)).unwrap().is_empty());
        let nulls = json!({ "idle_timeout": null, "formatting": null, "dbt_command": null });
        assert!(proxy_args(Some(&nulls)).unwrap().is_empty());
        assert_eq!(
            dbt_command(Some(&nulls)).unwrap(),
            ["dbt", "lsp", "--no-send-anonymous-usage-stats"]
        );
    }

    #[test]
    fn maps_settings_to_proxy_options() {
        let settings = json!({ "idle_timeout": 0, "formatting": true, "env_placeholders": false });
        assert_eq!(
            proxy_args(Some(&settings)).unwrap(),
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
        assert_eq!(dbt_command(None).unwrap(), expected);
        assert_eq!(
            dbt_command(Some(&json!({ "dbt_command": [] }))).unwrap(),
            expected
        );
    }

    #[test]
    fn replaces_the_dbt_command() {
        let settings = json!({ "dbt_command": ["uv", "run", "dbt", "lsp"] });
        assert_eq!(
            dbt_command(Some(&settings)).unwrap(),
            ["uv", "run", "dbt", "lsp"]
        );
    }

    #[test]
    fn reports_mistyped_settings() {
        assert_eq!(
            proxy_args(Some(&json!({ "idle_timeout": "1800" }))).unwrap_err(),
            r#"lsp.refmux.settings.idle_timeout must be a non-negative whole number of seconds, not "1800""#
        );
        assert!(proxy_args(Some(&json!({ "idle_timeout": -1 }))).is_err());
        assert!(proxy_args(Some(&json!({ "formatting": "true" }))).is_err());
        assert!(dbt_command(Some(&json!({ "dbt_command": "uv run dbt lsp" }))).is_err());
        let with_number = json!({ "dbt_command": ["dbt", "lsp", "--threads", 4] });
        assert!(dbt_command(Some(&with_number)).is_err());
    }
}
