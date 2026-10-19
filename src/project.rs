use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

pub const MANIFEST: &str = "dbt_project.yml";
const PROFILES: &str = "profiles.yml";
const PACKAGE_DIRS: [&str; 2] = ["dbt_packages", "dbt_modules"];

/// The nearest ancestor of `file` holding a `dbt_project.yml`. Installed packages carry their own
/// manifest but belong to the enclosing project, the way `node_modules` belongs to its tsconfig.
pub fn find_project(file: &Path) -> Option<PathBuf> {
    file.ancestors()
        .skip(1)
        .find(|dir| dir.join(MANIFEST).is_file() && !is_installed_package(dir))
        .map(Path::to_path_buf)
}

/// Packages ship their own nested projects too, such as `dbt_utils/integration_tests`.
fn is_installed_package(dir: &Path) -> bool {
    dir.ancestors()
        .skip(1)
        .filter(|root| root.join(MANIFEST).is_file())
        .any(|root| {
            package_dirs(root)
                .iter()
                .any(|packages| dir.starts_with(packages))
        })
}

/// Where `dbt deps` puts the packages of the project at `root`: `dbt_packages` (or the legacy
/// `dbt_modules`), and the directory `DBT_PACKAGES_INSTALL_PATH` or else the project's
/// `packages-install-path` names. A `--packages-install-path` in the server command is not seen.
fn package_dirs(root: &Path) -> Vec<PathBuf> {
    let configured = std::env::var_os("DBT_PACKAGES_INSTALL_PATH")
        .map(PathBuf::from)
        .or_else(|| {
            let manifest = std::fs::read_to_string(root.join(MANIFEST)).ok()?;
            let line = manifest
                .lines()
                .find_map(|line| line.strip_prefix("packages-install-path:"))?;
            let value = line.split(" #").next()?.trim().trim_matches(['"', '\'']);
            (!value.is_empty()).then(|| PathBuf::from(value))
        });
    PACKAGE_DIRS
        .iter()
        .map(PathBuf::from)
        .chain(configured)
        .map(|packages| root.join(packages))
        .collect()
}

pub struct Profiles {
    /// Passed as `--profiles-dir`; `None` leaves dbt's own lookup in charge.
    pub flag: Option<PathBuf>,
    /// The `profiles.yml` dbt will read, scanned for `env_var()` references.
    pub file: Option<PathBuf>,
}

/// Mirrors dbt's lookup: `--profiles-dir` in the server command, `DBT_PROFILES_DIR` from the
/// environment and then from the project's `.env`, a `profiles.yml` next to the project, then
/// `~/.dbt`. dbt resolves a relative directory against its working directory, the project.
pub fn profiles(dir: &Path, server_args: &[String]) -> Profiles {
    let explicit = server_args
        .iter()
        .enumerate()
        .find_map(|(index, arg)| match arg.strip_prefix("--profiles-dir") {
            Some("") => server_args.get(index + 1).map(PathBuf::from),
            Some(value) => value.strip_prefix('=').map(PathBuf::from),
            None => None,
        })
        .or_else(|| std::env::var_os("DBT_PROFILES_DIR").map(PathBuf::from))
        .or_else(|| {
            read_dotenv(dir)
                .into_iter()
                .find(|(key, _)| key == "DBT_PROFILES_DIR")
                .map(|(_, value)| PathBuf::from(value))
        });
    if let Some(explicit) = explicit {
        return Profiles {
            flag: None,
            file: Some(dir.join(explicit).join(PROFILES)),
        };
    }
    let local = dir.join(PROFILES);
    if local.is_file() {
        return Profiles {
            flag: Some(dir.to_path_buf()),
            file: Some(local),
        };
    }
    Profiles {
        flag: None,
        file: std::env::home_dir().map(|home| home.join(".dbt").join(PROFILES)),
    }
}

/// dbt refuses to render a profile when an `env_var('X')` without default is unset, and `dbt lsp`
/// then stays silent. A path in a per-user directory is a safe stand-in: static analysis opens the
/// warehouse only for hooks such as `on-run-start`, and a DuckDB database they create there harms
/// nothing. Where dbt needs a number or a boolean instead, the stand-in is `1` or `false`.
/// Variables from the environment or the project's `.env` (which dbt loads from its working
/// directory, the project) are left alone.
pub fn env_placeholders(dir: &Path, profiles_file: Option<&Path>) -> Vec<(String, String)> {
    let manifest = dir.join(MANIFEST);
    // One text, so that a default in either file counts for both.
    let text = [Some(manifest.as_path()), profiles_file]
        .into_iter()
        .flatten()
        .filter_map(|file| std::fs::read_to_string(file).ok())
        .collect::<Vec<_>>()
        .join("\n");
    let dotenv = read_dotenv(dir);
    let missing: Vec<(String, Kind)> = required_env_vars(&text)
        .into_iter()
        .filter(|(name, _)| {
            std::env::var_os(name).is_none() && !dotenv.iter().any(|(key, _)| key == name)
        })
        .collect();
    let base = placeholder_root().join(slug(dir));
    if missing.iter().any(|(_, kind)| *kind == Kind::Path) {
        let _ = std::fs::create_dir_all(&base);
    }
    missing
        .into_iter()
        .map(|(name, kind)| {
            let value = match kind {
                Kind::Path => base.join(&name).display().to_string(),
                Kind::Number => "1".to_owned(),
                Kind::Boolean => "false".to_owned(),
            };
            (name, value)
        })
        .collect()
}

/// Per-user, so another local user cannot plant files where dbt opens a database. The temp dir is
/// per-user on macOS and Windows; elsewhere it is usually the shared `/tmp`.
fn placeholder_root() -> PathBuf {
    let base = if cfg!(any(target_os = "macos", windows)) {
        std::env::temp_dir()
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .or_else(|| std::env::home_dir().map(|home| home.join(".cache")))
            .unwrap_or_else(std::env::temp_dir)
    };
    base.join("refmux")
}

fn slug(dir: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    dir.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// What a placeholder has to be for dbt to render the profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Path,
    Number,
    Boolean,
}

/// Names passed to `env_var(...)` without a default, with the kind of value each needs. A name
/// that another call defaults is left out: that call may be the active target's, which renders
/// without the variable, and a placeholder would override its default.
fn required_env_vars(text: &str) -> BTreeMap<String, Kind> {
    const CALL: &str = "env_var(";
    let (mut required, mut defaulted) = (BTreeMap::new(), BTreeSet::new());
    let mut offset = 0;
    while let Some(found) = text[offset..].find(CALL) {
        let start = offset + found;
        offset = start + CALL.len();
        let preceded_by_identifier = text[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if preceded_by_identifier {
            continue;
        }
        match string_argument(&text[offset..]) {
            Some((name, None)) => {
                defaulted.insert(name);
            }
            Some((name, Some(following))) => {
                let line_start = text[..start].rfind('\n').map_or(0, |newline| newline + 1);
                let kind = placeholder_kind(&text[line_start..start], following);
                let entry = required.entry(name).or_insert(kind);
                *entry = (*entry).max(kind);
            }
            None => {}
        }
    }
    required.retain(|name, _| !defaulted.contains(name));
    required
}

/// The quoted name that opens `rest`, and the text after the call when it has no default
/// (`None` when it has one).
fn string_argument(rest: &str) -> Option<(String, Option<&str>)> {
    let rest = rest.trim_start();
    let quote = rest.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    let rest = &rest[1..];
    let end = rest.find(quote)?;
    let name = &rest[..end];
    let after = rest[end + 1..].trim_start();
    let valid = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        return None;
    }
    if after.starts_with(',') {
        return Some((name.to_owned(), None));
    }
    let following = after.strip_prefix(')')?;
    Some((name.to_owned(), Some(following)))
}

/// dbt rejects a path where a profile needs a number or a boolean: behind `| as_number` or
/// `| as_bool`, and as `threads`.
fn placeholder_kind(line: &str, following: &str) -> Kind {
    let filter = following
        .trim_start()
        .strip_prefix('|')
        .map_or("", str::trim_start);
    if filter.starts_with("as_bool") {
        Kind::Boolean
    } else if filter.starts_with("as_number") || line.trim_start().starts_with("threads:") {
        Kind::Number
    } else {
        Kind::Path
    }
}

/// The project's `.env`, read even when it is not valid UTF-8, since dbt loads such a file too.
fn read_dotenv(dir: &Path) -> Vec<(String, String)> {
    std::fs::read(dir.join(".env"))
        .map(|bytes| dotenv(&String::from_utf8_lossy(&bytes)))
        .unwrap_or_default()
}

/// `KEY=value` entries the way dotenvy, which dbt uses, reads them: after a byte-order mark and
/// an `export` prefix, with quotes removed and `$VAR` or `${VAR}` expanded outside single quotes.
/// Unlike dotenvy, a line it cannot parse does not end the file.
fn dotenv(text: &str) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = Vec::new();
    for line in text.trim_start_matches('\u{feff}').lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line
            .strip_prefix("export")
            .filter(|rest| rest.starts_with(char::is_whitespace))
            .map_or(line, str::trim_start);
        let Some((key, raw)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !key.is_empty() {
            let value = dotenv_value(raw.trim(), &entries);
            entries.push((key.to_owned(), value));
        }
    }
    entries
}

fn dotenv_value(raw: &str, earlier: &[(String, String)]) -> String {
    for quote in ['\'', '"'] {
        if let Some(rest) = raw.strip_prefix(quote)
            && let Some(end) = rest.find(quote)
        {
            let inner = &rest[..end];
            return if quote == '\'' {
                inner.to_owned()
            } else {
                expand(inner, earlier)
            };
        }
    }
    expand(
        raw.split(" #").next().unwrap_or_default().trim_end(),
        earlier,
    )
}

/// `$NAME` and `${NAME}` from the environment, else from an earlier entry, as dotenvy does.
fn expand(value: &str, earlier: &[(String, String)]) -> String {
    let mut expanded = String::new();
    let mut rest = value;
    while let Some(dollar) = rest.find('$') {
        expanded.push_str(&rest[..dollar]);
        rest = &rest[dollar + 1..];
        let (name, after) = match rest.strip_prefix('{') {
            Some(braced) => braced.split_once('}').unwrap_or((braced, "")),
            None => rest.split_at(
                rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(rest.len()),
            ),
        };
        if name.is_empty() {
            expanded.push('$');
        } else if let Ok(value) = std::env::var(name) {
            expanded.push_str(&value);
        } else if let Some((_, value)) = earlier.iter().rev().find(|(key, _)| key == name) {
            expanded.push_str(value);
        }
        rest = after;
    }
    expanded.push_str(rest);
    expanded
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("refmux-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
    }

    #[test]
    fn finds_the_nearest_project_and_skips_installed_packages() {
        let root = scratch("find");
        let project = root.join("demos/a/pipeline/dbt");
        touch(&project.join(MANIFEST));
        touch(&project.join("dbt_packages/dbt_utils").join(MANIFEST));
        touch(
            &project
                .join("dbt_packages/dbt_utils/integration_tests")
                .join(MANIFEST),
        );
        let nested = project.join("nested");
        touch(&nested.join(MANIFEST));

        assert_eq!(
            find_project(&project.join("models/m.sql")),
            Some(project.clone())
        );
        assert_eq!(find_project(&project.join(MANIFEST)), Some(project.clone()));
        assert_eq!(
            find_project(&project.join("dbt_packages/dbt_utils/macros/x.sql")),
            Some(project.clone())
        );
        assert_eq!(
            find_project(&project.join("dbt_packages/dbt_utils/integration_tests/models/x.sql")),
            Some(project.clone())
        );
        assert_eq!(find_project(&nested.join("models/m.sql")), Some(nested));
        assert_eq!(find_project(&root.join("apps/app/schema.sql")), None);
        if std::env::var_os("DBT_PACKAGES_INSTALL_PATH").is_none() {
            let custom = root.join("custom");
            touch(&custom.join(MANIFEST));
            fs::write(
                custom.join(MANIFEST),
                "name: custom\npackages-install-path: \"vendor\" # dbt deps\n",
            )
            .unwrap();
            touch(&custom.join("vendor/pkg").join(MANIFEST));
            assert_eq!(
                find_project(&custom.join("vendor/pkg/models/x.sql")),
                Some(custom)
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn takes_the_profiles_dir_from_the_server_command() {
        let project = Path::new("/repo/project");
        let args = |args: &[&str]| args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        for server_args in [
            args(&["lsp", "--profiles-dir", "conf"]),
            args(&["lsp", "--profiles-dir=conf"]),
        ] {
            let found = profiles(project, &server_args);
            assert_eq!(found.flag, None);
            assert_eq!(found.file, Some(project.join("conf").join(PROFILES)));
        }
        let absolute = profiles(project, &args(&["lsp", "--profiles-dir", "/etc/dbt"]));
        assert_eq!(
            absolute.file,
            Some(PathBuf::from("/etc/dbt").join(PROFILES))
        );
    }

    #[test]
    fn takes_the_profiles_dir_from_the_project_dotenv() {
        if std::env::var_os("DBT_PROFILES_DIR").is_some() {
            return;
        }
        let project = scratch("profiles");
        touch(&project.join(PROFILES));
        fs::write(project.join(".env"), "DBT_PROFILES_DIR=\"conf\"\n").unwrap();
        let found = profiles(&project, &[]);
        assert_eq!(found.flag, None);
        assert_eq!(found.file, Some(project.join("conf").join(PROFILES)));
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn collects_env_vars_without_default() {
        let text = r#"
            path: "{{ env_var('WAREHOUSE_DUCKDB_PATH') }}"
            user: "{{ env_var( "DB_USER" ) }}"
            pass: "{{ env_var('DB_PASS', 'secret') }}"
            other: "{{ get_env_var('NOT_DBT') }}"
            bad: "{{ env_var(name) }}"
        "#;
        let names: Vec<String> = required_env_vars(text).into_keys().collect();
        assert_eq!(names, ["DB_USER", "WAREHOUSE_DUCKDB_PATH"]);
    }

    #[test]
    fn leaves_out_variables_another_call_defaults() {
        let text = r#"
            dev:
              path: "{{ env_var('WAREHOUSE_PATH', 'dev.duckdb') }}"
            prod:
              path: "{{ env_var('WAREHOUSE_PATH') }}"
              token: "{{ env_var('PROD_TOKEN') }}"
        "#;
        let names: Vec<String> = required_env_vars(text).into_keys().collect();
        assert_eq!(names, ["PROD_TOKEN"]);
    }

    #[test]
    fn gives_numbers_and_booleans_where_dbt_needs_them() {
        let text = r#"
            threads: "{{ env_var('RMX_THREADS') }}"
            port: "{{ env_var('RMX_PORT') | as_number }}"
            keepalive: "{{ env_var('RMX_KEEPALIVE') | as_bool }}"
            path: "{{ env_var('RMX_PATH') }}"
        "#;
        let kinds: Vec<(String, Kind)> = required_env_vars(text).into_iter().collect();
        let expected = [
            ("RMX_KEEPALIVE", Kind::Boolean),
            ("RMX_PATH", Kind::Path),
            ("RMX_PORT", Kind::Number),
            ("RMX_THREADS", Kind::Number),
        ];
        assert_eq!(kinds, expected.map(|(name, kind)| (name.to_owned(), kind)));
    }

    #[test]
    fn reads_dotenv_entries() {
        let text = "\u{feff}RMX_A=1\n# comment\nexport RMX_B = 2\nexport\tRMX_C='$RMX_A'\nRMX_D=\"$RMX_A/x\" # d\nRMX_E=${RMX_A}/y # e\n\nRMX_F\n =x\n";
        let entries = dotenv(text);
        let pairs: Vec<(&str, &str)> = entries
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("RMX_A", "1"),
                ("RMX_B", "2"),
                ("RMX_C", "$RMX_A"),
                ("RMX_D", "1/x"),
                ("RMX_E", "1/y")
            ]
        );
    }

    #[test]
    fn fills_only_unset_variables() {
        let project = scratch("env");
        fs::write(
            project.join(MANIFEST),
            "name: p\nvars:\n  x: \"{{ env_var('PATH') }}\"\n",
        )
        .unwrap();
        fs::write(
            project.join(PROFILES),
            "p:\n  outputs:\n    dev:\n      path: \"{{ env_var('REFMUX_TEST_MISSING') }}\"\n      token: \"{{ env_var('REFMUX_TEST_DOTENV') }}\"\n",
        )
        .unwrap();
        fs::write(project.join(".env"), "REFMUX_TEST_DOTENV=x\n").unwrap();

        let filled = env_placeholders(&project, Some(&project.join(PROFILES)));
        let names: Vec<&str> = filled.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["REFMUX_TEST_MISSING"]);
        assert!(Path::new(&filled[0].1).starts_with(placeholder_root()));
        let _ = fs::remove_dir_all(placeholder_root().join(slug(&project)));
        fs::remove_dir_all(project).unwrap();
    }
}
