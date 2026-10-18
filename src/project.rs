use std::collections::BTreeSet;
use std::collections::hash_map::DefaultHasher;
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

fn is_installed_package(dir: &Path) -> bool {
    dir.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| PACKAGE_DIRS.contains(&name))
}

pub struct Profiles {
    /// Passed as `--profiles-dir`; `None` leaves dbt's own lookup in charge.
    pub flag: Option<PathBuf>,
    /// The `profiles.yml` dbt will read, scanned for `env_var()` references.
    pub file: Option<PathBuf>,
}

/// Mirrors dbt's lookup: `DBT_PROFILES_DIR`, a `profiles.yml` next to the project, then `~/.dbt`.
pub fn profiles(dir: &Path) -> Profiles {
    if let Some(env_dir) = std::env::var_os("DBT_PROFILES_DIR") {
        return Profiles {
            flag: None,
            file: Some(PathBuf::from(env_dir).join(PROFILES)),
        };
    }
    let local = dir.join(PROFILES);
    if local.is_file() {
        return Profiles {
            flag: Some(dir.to_path_buf()),
            file: Some(local),
        };
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    Profiles {
        flag: None,
        file: home.map(|home| PathBuf::from(home).join(".dbt").join(PROFILES)),
    }
}

/// dbt refuses to render a profile when an `env_var('X')` without default is unset, and `dbt lsp`
/// then stays silent. Static analysis never opens the warehouse, so a path under the temp dir is a
/// safe stand-in. Variables from the environment or the project's `.env` (which dbt loads from
/// its working directory, the project) are left alone.
pub fn env_placeholders(dir: &Path, profiles_file: Option<&Path>) -> Vec<(String, String)> {
    let manifest = dir.join(MANIFEST);
    let required: BTreeSet<String> = [Some(manifest.as_path()), profiles_file]
        .into_iter()
        .flatten()
        .filter_map(|file| std::fs::read_to_string(file).ok())
        .flat_map(|text| required_env_vars(&text))
        .collect();
    let defined = std::fs::read_to_string(dir.join(".env"))
        .map(|text| dotenv_keys(&text))
        .unwrap_or_default();
    let missing: Vec<String> = required
        .into_iter()
        .filter(|name| std::env::var_os(name).is_none() && !defined.contains(name))
        .collect();
    if missing.is_empty() {
        return Vec::new();
    }
    let base = std::env::temp_dir().join("refmux").join(slug(dir));
    let _ = std::fs::create_dir_all(&base);
    missing
        .into_iter()
        .map(|name| {
            let value = base.join(&name).display().to_string();
            (name, value)
        })
        .collect()
}

fn slug(dir: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    dir.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Names passed to `env_var(...)` with a single argument, i.e. without a default.
pub fn required_env_vars(text: &str) -> BTreeSet<String> {
    const CALL: &str = "env_var(";
    let mut names = BTreeSet::new();
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
        if let Some(name) = single_string_argument(&text[offset..]) {
            names.insert(name);
        }
    }
    names
}

fn single_string_argument(rest: &str) -> Option<String> {
    let rest = rest.trim_start();
    let quote = rest.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    let rest = &rest[1..];
    let end = rest.find(quote)?;
    let name = &rest[..end];
    let closes = rest[end + 1..].trim_start().starts_with(')');
    let valid = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    (closes && valid).then(|| name.to_owned())
}

pub fn dotenv_keys(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (key, _) = line.split_once('=')?;
            let key = key.trim();
            (!key.is_empty()).then(|| key.to_owned())
        })
        .collect()
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
        assert_eq!(find_project(&nested.join("models/m.sql")), Some(nested));
        assert_eq!(find_project(&root.join("apps/app/schema.sql")), None);
        fs::remove_dir_all(root).unwrap();
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
        let names: Vec<String> = required_env_vars(text).into_iter().collect();
        assert_eq!(names, ["DB_USER", "WAREHOUSE_DUCKDB_PATH"]);
    }

    #[test]
    fn reads_dotenv_keys() {
        let keys = dotenv_keys("A=1\n# comment\nexport B = 2\n\nC\n =x\n");
        assert_eq!(keys.into_iter().collect::<Vec<_>>(), ["A", "B"]);
    }

    #[test]
    fn fills_only_unset_variables() {
        let project = scratch("env");
        fs::write(
            project.join(MANIFEST),
            "name: p\nvars:\n  x: \"{{ env_var('REFMUX_TEST_FROM_ENV') }}\"\n",
        )
        .unwrap();
        fs::write(
            project.join(PROFILES),
            "p:\n  outputs:\n    dev:\n      path: \"{{ env_var('REFMUX_TEST_MISSING') }}\"\n      token: \"{{ env_var('REFMUX_TEST_DOTENV') }}\"\n",
        )
        .unwrap();
        fs::write(project.join(".env"), "REFMUX_TEST_DOTENV=x\n").unwrap();
        // SAFETY: only this test reads or writes this variable name.
        unsafe { std::env::set_var("REFMUX_TEST_FROM_ENV", "1") };

        let filled = env_placeholders(&project, Some(&project.join(PROFILES)));
        let names: Vec<&str> = filled.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["REFMUX_TEST_MISSING"]);
        assert!(Path::new(&filled[0].1).starts_with(std::env::temp_dir()));
        fs::remove_dir_all(project).unwrap();
    }
}
