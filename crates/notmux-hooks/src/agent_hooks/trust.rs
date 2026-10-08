//! Per-folder trust for unattended agent launches.
//!
//! Every harness asks once before it works in a folder it has not seen.
//! A managed worker cannot answer, so the runtime writes the vendor's own
//! trust record before the launch, exactly the way Warp's harness drivers
//! do (`../warp-main/app/src/ai/agent_sdk/driver/harness/{claude_code,codex}.rs`):
//!
//! * Claude Code: `projects.<cwd>.hasTrustDialogAccepted = true` and
//!   `hasCompletedOnboarding = true` in `.claude.json`, plus
//!   `skipDangerousModePermissionPrompt = true` in `settings.json` so
//!   `--dangerously-skip-permissions` does not stop at its own confirmation.
//! * Codex: `[projects."<canonical cwd>"] trust_level = "trusted"` and
//!   `check_for_update_on_startup = false` in `config.toml`. Codex's trust
//!   check is not recursive, so immediate child git repositories are
//!   trusted too. When a `model` is pinned, `[notice.model_migrations]`
//!   gets an entry for it so the model-upgrade notice cannot block.
//!
//! Edits go through [`super::config::ConfigFile`]: locked, staged, atomic,
//! never over a concurrent user edit, and refused on a read-only file.

use super::config::{ConfigFile, commit};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// One key the runtime set. Recorded on the run so the user can see and
/// undo what happened to their vendor configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustWrite {
    pub path: PathBuf,
    pub key: String,
    pub value: String,
}

fn home() -> Result<PathBuf, String> {
    super::home_dir().ok_or_else(|| "HOME not set".to_string())
}

/// `(.claude.json, settings.json)`. With `CLAUDE_CONFIG_DIR` both live in
/// that directory; otherwise `.claude.json` sits directly in `$HOME` and
/// `settings.json` in `~/.claude`.
pub fn claude_paths() -> Result<(PathBuf, PathBuf), String> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        let dir = PathBuf::from(dir);
        return Ok((dir.join(".claude.json"), dir.join("settings.json")));
    }
    let home = home()?;
    Ok((home.join(".claude.json"), home.join(".claude").join("settings.json")))
}

pub fn codex_config_path() -> Result<PathBuf, String> {
    let dir = std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or(home()?.join(".codex"));
    Ok(dir.join("config.toml"))
}

pub fn trust_claude_project(cwd: &Path) -> Result<Vec<TrustWrite>, String> {
    let (json_path, settings_path) = claude_paths()?;
    trust_claude_project_at(&json_path, &settings_path, cwd)
}

pub fn trust_codex_project(cwd: &Path) -> Result<Vec<TrustWrite>, String> {
    trust_codex_project_at(&codex_config_path()?, cwd)
}

/// The folder keys a vendor may look up: the path as given and its
/// canonical form, when they differ.
fn folder_keys(cwd: &Path) -> Vec<String> {
    let given = cwd.to_string_lossy().into_owned();
    let mut keys = vec![given.clone()];
    if let Ok(canonical) = cwd.canonicalize() {
        let canonical = canonical.to_string_lossy().into_owned();
        if canonical != given {
            keys.push(canonical);
        }
    }
    keys
}

pub fn trust_claude_project_at(
    json_path: &Path,
    settings_path: &Path,
    cwd: &Path,
) -> Result<Vec<TrustWrite>, String> {
    let json_file = ConfigFile::load(json_path)?;
    let settings_file = ConfigFile::load(settings_path)?;
    let json_original = json_file.json()?;
    let settings_original = settings_file.json()?;
    let mut writes = Vec::new();

    let mut doc = json_original.clone();
    doc["hasCompletedOnboarding"] = json!(true);
    writes.push(TrustWrite {
        path: json_file.path.clone(),
        key: "hasCompletedOnboarding".into(),
        value: "true".into(),
    });
    if !doc.get("projects").is_some_and(Value::is_object) {
        if doc.get("projects").is_some_and(|p| !p.is_null()) {
            return Err(format!("{}: projects is not an object; not touching it", json_file.path.display()));
        }
        doc["projects"] = json!({});
    }
    for key in folder_keys(cwd) {
        let project = &mut doc["projects"][&key];
        if project.is_null() {
            *project = json!({});
        } else if !project.is_object() {
            return Err(format!("{}: projects[{key:?}] is not an object; not touching it", json_file.path.display()));
        }
        project["hasTrustDialogAccepted"] = json!(true);
        writes.push(TrustWrite {
            path: json_file.path.clone(),
            key: format!("projects[{key:?}].hasTrustDialogAccepted"),
            value: "true".into(),
        });
    }

    let mut settings = settings_original.clone();
    settings["skipDangerousModePermissionPrompt"] = json!(true);
    writes.push(TrustWrite {
        path: settings_file.path.clone(),
        key: "skipDangerousModePermissionPrompt".into(),
        value: "true".into(),
    });

    let mut changes = Vec::new();
    if doc != json_original {
        changes.push((&json_file, serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?));
    }
    if settings != settings_original {
        changes.push((&settings_file, serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?));
    }
    commit(&changes)?;
    Ok(writes)
}

fn toml_table<'a>(
    parent: &'a mut toml_edit::Table,
    key: &str,
    path: &Path,
) -> Result<&'a mut toml_edit::Table, String> {
    if !parent.contains_key(key) {
        let mut table = toml_edit::Table::new();
        table.set_implicit(true);
        parent.insert(key, toml_edit::Item::Table(table));
    }
    parent
        .get_mut(key)
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| format!("{}: {key} is not a table; not touching it", path.display()))
}

fn set_codex_trust(doc: &mut toml_edit::DocumentMut, key: &str, path: &Path) -> Result<(), String> {
    let projects = toml_table(doc.as_table_mut(), "projects", path)?;
    let project = toml_table(projects, key, path)?;
    project.set_implicit(false);
    project["trust_level"] = toml_edit::value("trusted");
    Ok(())
}

pub fn trust_codex_project_at(config_path: &Path, cwd: &Path) -> Result<Vec<TrustWrite>, String> {
    let file = ConfigFile::load(config_path)?;
    let original = file.original.clone().unwrap_or_default();
    let mut doc = original
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("{} is not valid TOML ({e}); not touching it", file.path.display()))?;
    let mut writes = Vec::new();

    let canonical = cwd
        .canonicalize()
        .map_err(|e| format!("cannot canonicalize {}: {e}", cwd.display()))?;
    let mut keys = vec![canonical.to_string_lossy().into_owned()];
    if let Ok(entries) = std::fs::read_dir(&canonical) {
        for entry in entries.flatten() {
            let child = entry.path();
            if child.is_dir() && child.join(".git").exists() {
                keys.push(child.to_string_lossy().into_owned());
            }
        }
    }
    for key in &keys {
        set_codex_trust(&mut doc, key, &file.path)?;
        writes.push(TrustWrite {
            path: file.path.clone(),
            key: format!("projects.{key:?}.trust_level"),
            value: "trusted".into(),
        });
    }

    if doc.get("check_for_update_on_startup").is_some_and(|v| !v.is_bool()) {
        return Err(format!("{}: check_for_update_on_startup is not a boolean; not touching it", file.path.display()));
    }
    doc["check_for_update_on_startup"] = toml_edit::value(false);
    writes.push(TrustWrite {
        path: file.path.clone(),
        key: "check_for_update_on_startup".into(),
        value: "false".into(),
    });

    if let Some(model) = doc.get("model").and_then(toml_edit::Item::as_str).map(str::to_string) {
        let notice = toml_table(doc.as_table_mut(), "notice", &file.path)?;
        let migrations = toml_table(notice, "model_migrations", &file.path)?;
        migrations.set_implicit(false);
        if !migrations.contains_key(&model) {
            migrations[&model] = toml_edit::value(model.as_str());
        }
        writes.push(TrustWrite {
            path: file.path.clone(),
            key: format!("notice.model_migrations.{model:?}"),
            value: migrations
                .get(&model)
                .and_then(toml_edit::Item::as_str)
                .unwrap_or(&model)
                .to_string(),
        });
    }

    let updated = doc.to_string();
    if updated != original {
        commit(&[(&file, updated)])?;
    }
    Ok(writes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_trust_keeps_unrelated_keys_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join(".claude.json");
        let settings = dir.path().join("settings.json");
        std::fs::write(&json, r#"{"theme":"dark","projects":{"/other":{"allowedTools":["Bash"]}}}"#).unwrap();
        std::fs::write(&settings, r#"{"hooks":{"Stop":[]},"model":"opus"}"#).unwrap();
        let cwd = dir.path().join("work");
        std::fs::create_dir(&cwd).unwrap();

        let writes = trust_claude_project_at(&json, &settings, &cwd).unwrap();
        assert!(writes.iter().any(|w| w.key.contains("hasTrustDialogAccepted")));
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
        assert_eq!(doc["theme"], "dark");
        assert_eq!(doc["projects"]["/other"]["allowedTools"][0], "Bash");
        assert_eq!(doc["hasCompletedOnboarding"], true);
        assert_eq!(doc["projects"][cwd.to_str().unwrap()]["hasTrustDialogAccepted"], true);
        let s: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(s["model"], "opus");
        assert_eq!(s["skipDangerousModePermissionPrompt"], true);

        let first = std::fs::read_to_string(&json).unwrap();
        trust_claude_project_at(&json, &settings, &cwd).unwrap();
        assert_eq!(std::fs::read_to_string(&json).unwrap(), first, "second run changes nothing");
    }

    #[test]
    fn claude_trust_creates_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join("cfg").join(".claude.json");
        let settings = dir.path().join("cfg").join("settings.json");
        trust_claude_project_at(&json, &settings, dir.path()).unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
        assert_eq!(doc["projects"][dir.path().to_str().unwrap()]["hasTrustDialogAccepted"], true);
        assert!(settings.exists());
    }

    #[cfg(unix)]
    #[test]
    fn read_only_claude_config_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join(".claude.json");
        let settings = dir.path().join("settings.json");
        std::fs::write(&json, "{}").unwrap();
        std::fs::set_permissions(&json, std::fs::Permissions::from_mode(0o444)).unwrap();
        let err = trust_claude_project_at(&json, &settings, dir.path()).unwrap_err();
        assert!(err.contains("read-only"), "{err}");
        assert_eq!(std::fs::read_to_string(&json).unwrap(), "{}");
    }

    #[test]
    fn codex_trust_keeps_unrelated_keys_trusts_child_repos_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            "model = \"gpt-5\"\nsandbox_mode = \"workspace-write\"\n\n[projects.\"/other\"]\ntrust_level = \"untrusted\"\n",
        )
        .unwrap();
        let cwd = dir.path().join("work");
        std::fs::create_dir_all(cwd.join("child").join(".git")).unwrap();
        std::fs::create_dir_all(cwd.join("plain")).unwrap();

        let writes = trust_codex_project_at(&config, &cwd).unwrap();
        let text = std::fs::read_to_string(&config).unwrap();
        let doc: toml_edit::DocumentMut = text.parse().unwrap();
        let canonical = cwd.canonicalize().unwrap();
        let key = canonical.to_string_lossy().into_owned();
        assert_eq!(doc["projects"][&key]["trust_level"].as_str(), Some("trusted"));
        let child_key = canonical.join("child").to_string_lossy().into_owned();
        assert_eq!(doc["projects"][&child_key]["trust_level"].as_str(), Some("trusted"));
        assert!(doc["projects"].get(&canonical.join("plain").to_string_lossy().into_owned()).is_none());
        assert_eq!(doc["projects"]["/other"]["trust_level"].as_str(), Some("untrusted"));
        assert_eq!(doc["sandbox_mode"].as_str(), Some("workspace-write"));
        assert_eq!(doc["check_for_update_on_startup"].as_bool(), Some(false));
        assert_eq!(doc["notice"]["model_migrations"]["gpt-5"].as_str(), Some("gpt-5"));
        assert!(writes.iter().any(|w| w.key == "check_for_update_on_startup"));

        trust_codex_project_at(&config, &cwd).unwrap();
        assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
    }

    #[test]
    fn codex_trust_without_pinned_model_writes_no_migration_notice() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        trust_codex_project_at(&config, dir.path()).unwrap();
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(!text.contains("model_migrations"), "{text}");
        assert!(text.contains("trust_level = \"trusted\""));
    }

    #[test]
    fn codex_trust_refuses_broken_toml() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "model = [broken\n").unwrap();
        let err = trust_codex_project_at(&config, dir.path()).unwrap_err();
        assert!(err.contains("not valid TOML"), "{err}");
    }
}
