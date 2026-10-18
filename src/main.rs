mod mux;
mod project;
mod rpc;

use std::process::ExitCode;
use std::time::Duration;

pub struct Config {
    pub server_command: Vec<String>,
    pub idle_timeout: Option<Duration>,
    pub formatting: bool,
    pub env_placeholders: bool,
}

const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(600);

const USAGE: &str = "\
Usage: refmux [OPTIONS] [-- <server command>...]

Runs one dbt Fusion language server per dbt project and routes every document to the
server of its nearest dbt_project.yml. The server command defaults to `dbt lsp`;
--project-dir (and --profiles-dir when the project has a profiles.yml) are appended.

Options:
  --idle-timeout <seconds>  Stop a project's server after it has had no open documents
                            for this long (default 600, 0 keeps servers running)
  --formatting              Advertise dbt's document formatting (off by default, so other
                            SQL and YAML formatters keep handling format-on-save)
  --no-env-placeholders     Do not fill unset env_var() references with placeholder paths
  -h, --help                Print this help
  -V, --version             Print the version
";

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Option<Config>, String> {
    let mut config = Config {
        server_command: Vec::new(),
        idle_timeout: Some(DEFAULT_IDLE_TIMEOUT),
        formatting: false,
        env_placeholders: true,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--" => {
                config.server_command = args.by_ref().collect();
                break;
            }
            "--idle-timeout" => {
                let value = args.next().ok_or("--idle-timeout needs a value")?;
                let seconds: u64 = value
                    .parse()
                    .map_err(|_| format!("--idle-timeout: not a number of seconds: {value}"))?;
                config.idle_timeout = (seconds > 0).then(|| Duration::from_secs(seconds));
            }
            "--formatting" => config.formatting = true,
            "--no-env-placeholders" => config.env_placeholders = false,
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("refmux {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }
    if config.server_command.is_empty() {
        config.server_command = vec!["dbt".into(), "lsp".into()];
    }
    Ok(Some(config))
}

fn main() -> ExitCode {
    match parse_args(std::env::args().skip(1)) {
        Ok(Some(config)) => mux::run(config),
        Ok(None) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("refmux: {message}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Config {
        parse_args(args.iter().map(|arg| arg.to_string()))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn defaults_to_dbt_lsp() {
        let config = parse(&[]);
        assert_eq!(config.server_command, ["dbt", "lsp"]);
        assert_eq!(config.idle_timeout, Some(DEFAULT_IDLE_TIMEOUT));
        assert!(!config.formatting);
        assert!(config.env_placeholders);
    }

    #[test]
    fn keeps_everything_after_the_separator_as_the_server_command() {
        let config = parse(&[
            "--idle-timeout",
            "0",
            "--formatting",
            "--",
            "mise",
            "exec",
            "dbt-fusion",
            "--",
            "dbt",
            "lsp",
        ]);
        assert_eq!(
            config.server_command,
            ["mise", "exec", "dbt-fusion", "--", "dbt", "lsp"]
        );
        assert_eq!(config.idle_timeout, None);
        assert!(config.formatting);
    }

    #[test]
    fn rejects_unknown_options() {
        assert!(parse_args(["--bogus".to_string()].into_iter()).is_err());
    }
}
