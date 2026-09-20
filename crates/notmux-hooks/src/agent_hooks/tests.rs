use super::*;
use serde_json::{Value, json};
use std::path::Path;

// Each installer invocation gets a private process environment, including HOME.
// No test installs hooks into a developer's real agent configuration.
#[test]
fn failed_later_publish_restores_the_original_file() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("settings.json");
    std::fs::write(&path, "{ \"model\": \"original\" }").unwrap();
    let file = config::ConfigFile::load(&path).unwrap();
    // The alias deterministically changes the later target between validation
    // and publication, without a timing-dependent background writer.
    assert!(config::commit(&[(&file, "{\"first\":true}".into()), (&file, "{\"second\":true}".into())]).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ \"model\": \"original\" }");
}

#[test]
fn uninstall_leaves_empty_and_unrelated_documents_byte_identical() {
    let home = tempfile::tempdir().unwrap();
    for (relative, action) in [("claude/settings.json", "uninstall-claude"), ("codex/hooks.json", "uninstall-codex")] {
        let path = home.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for original in ["{ \"hooks\": {} }", "{ \"hooks\": {\"Stop\": []} }", "{ \"model\": \"custom\" }"] {
            std::fs::write(&path, original).unwrap();
            invoke(home.path(), action, false);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
    }
}

#[test]
fn legacy_own_hooks_are_replaced_without_removing_mixed_user_handlers() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("claude/settings.json");
    let legacy = claude_hooks("/old installation/notmux")["hooks"]["Stop"][0]["hooks"][0].clone();
    let foreign = json!({"type": "command", "command": "[ -n \"$NOTMUX_SURFACE_ID\" ] && \"/user/notmux\" notify --body custom >/dev/null 2>&1 || true"});
    write_json(&path, &json!({"hooks": {"Stop": [{"matcher": "user-matcher", "custom": 123, "hooks": [legacy, foreign.clone()]}]}}));
    invoke(home.path(), "install-claude", false);
    let installed = read_json(&path);
    assert_eq!(installed["hooks"]["Stop"].as_array().unwrap().len(), 2);
    assert_eq!(installed["hooks"]["Stop"][0]["hooks"], json!([foreign.clone()]));
    assert_eq!(installed["hooks"]["Stop"][0]["custom"], 123);
    invoke(home.path(), "uninstall-claude", false);
    assert_eq!(read_json(&path), json!({"hooks": {"Stop": [{"matcher": "user-matcher", "custom": 123, "hooks": [foreign]}]}}));
}

#[test]
fn codex_remaps_foreign_trust_and_only_trusts_its_own_handlers() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("codex/hooks.json");
    let legacy = codex_hooks("/old installation/notmux")["hooks"]["Stop"][0]["hooks"][0].clone();
    let user = |name: &str| json!({"type": "command", "command": format!("echo {name}")});
    write_json(&path, &json!({"hooks": {"Stop": [
        {"matcher": "user-matcher", "hooks": [user("a"), legacy.clone(), user("b")]},
        {"hooks": [legacy]}, {"hooks": [user("c")]}
    ]}}));
    let key = std::fs::canonicalize(&path).unwrap().to_string_lossy().to_string();
    let config_path = home.path().join("codex/config.toml");
    let mut config = toml_edit::DocumentMut::new();
    config["features"]["hooks"] = toml_edit::value(false);
    for (suffix, hash) in [("0:0", "user-a"), ("0:1", "old-owned"), ("0:2", "user-b"), ("1:0", "old-owned"), ("2:0", "user-c")] {
        config["hooks"]["state"][format!("{key}:stop:{suffix}")]["trusted_hash"] = toml_edit::value(hash);
    }
    config["hooks"]["state"]["unrelated:path"]["trusted_hash"] = toml_edit::value("untouched");
    config["hooks"]["state"][format!("{key}:stop:0:1")]["custom_option"] = toml_edit::value("keep");
    std::fs::write(&config_path, config.to_string()).unwrap();
    invoke(home.path(), "install-codex", false);
    let installed = read_json(&path);
    assert_eq!(installed["hooks"]["Stop"][0]["hooks"], json!([user("a"), user("b")]));
    assert_eq!(installed["hooks"]["Stop"][1]["hooks"], json!([user("c")]));
    let config = std::fs::read_to_string(&config_path).unwrap().parse::<toml_edit::DocumentMut>().unwrap();
    assert_eq!(config["features"]["hooks"].as_bool(), Some(false));
    for (suffix, hash) in [("0:0", "user-a"), ("0:1", "user-b"), ("1:0", "user-c")] {
        assert_eq!(config["hooks"]["state"][format!("{key}:stop:{suffix}")]["trusted_hash"].as_str(), Some(hash));
    }
    let own = &installed["hooks"]["Stop"][2]["hooks"][0];
    let hash = codex_hook_trust_hash("stop", own["command"].as_str().unwrap(), CODEX_HOOK_TIMEOUT_MS);
    assert_eq!(config["hooks"]["state"][format!("{key}:stop:2:0")]["trusted_hash"].as_str(), Some(hash.as_str()));
    assert_eq!(config["hooks"]["state"][format!("{key}:stop:2:0")]["custom_option"].as_str(), Some("keep"));
    invoke(home.path(), "uninstall-codex", false);
    let config = std::fs::read_to_string(&config_path).unwrap().parse::<toml_edit::DocumentMut>().unwrap();
    assert!(config["hooks"]["state"].get(&format!("{key}:stop:2:0")).is_none());
    assert_eq!(config["hooks"]["state"]["unrelated:path"]["trusted_hash"].as_str(), Some("untouched"));
    assert_eq!(config["hooks"]["state"].as_table_like().unwrap().len(), 4);
}

#[test]
fn codex_refuses_to_overwrite_an_unowned_trust_key() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("codex/hooks.json");
    write_json(&path, &json!({"hooks": {"Stop": [{"hooks": [{"type":"command", "command":"echo user"}]}]}}));
    let original = std::fs::read(&path).unwrap();
    let key = std::fs::canonicalize(&path).unwrap().to_string_lossy().to_string();
    let config_path = home.path().join("codex/config.toml");
    let mut config = toml_edit::DocumentMut::new();
    config["hooks"]["state"][format!("{key}:stop:1:0")]["trusted_hash"] = toml_edit::value("user-owned");
    let config = config.to_string();
    std::fs::write(&config_path, &config).unwrap();
    invoke(home.path(), "install-codex", true);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), config);
}

#[test]
fn concurrent_installer_and_external_edits_are_not_overwritten() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("claude/settings.json");
    write_json(&path, &json!({"model":"original"}));
    let file = config::ConfigFile::load(&path).unwrap();
    invoke(home.path(), "install-claude", true);
    std::fs::write(&path, "{\"model\":\"edited\"}").unwrap();
    assert!(config::commit(&[(&file, "{}".into())]).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"model\":\"edited\"}");
    drop(file);
    invoke(home.path(), "install-claude", false);
    assert_eq!(read_json(&path)["model"], "edited");
}

#[cfg(unix)]
#[test]
fn symlinks_and_file_permissions_survive_installation() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("claude/settings.json");
    let target = home.path().join("managed-settings.json");
    write_json(&target, &json!({"model":"custom"}));
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    symlink(&target, &path).unwrap();
    invoke(home.path(), "install-claude", false);
    assert!(std::fs::symlink_metadata(&path).unwrap().is_symlink());
    assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o640);
    invoke(home.path(), "uninstall-claude", false);
    assert!(std::fs::symlink_metadata(&path).unwrap().is_symlink());
    assert_eq!(read_json(&target), json!({"model":"custom"}));
}

#[cfg(unix)]
#[test]
fn codex_read_only_config_leaves_both_files_unchanged() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("codex/hooks.json");
    let config_path = home.path().join("codex/config.toml");
    write_json(&path, &json!({"extra":"keep"}));
    std::fs::write(&config_path, "model = 'custom'\n").unwrap();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o444)).unwrap();
    let original = std::fs::read(&path).unwrap();
    invoke(home.path(), "install-codex", true);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), "model = 'custom'\n");
}

#[test]
fn isolated_installer() {
    let Ok(action) = std::env::var("NOTMUX_HOOK_TEST_ACTION") else {
        return;
    };
    let result = match action.as_str() {
        "install-claude" => install_claude(),
        "uninstall-claude" => uninstall_claude(),
        "install-codex" => install_codex(),
        "uninstall-codex" => uninstall_codex(),
        _ => panic!("unknown test action"),
    };
    if std::env::var("NOTMUX_HOOK_TEST_ERROR").as_deref() == Ok("1") {
        assert!(result.is_err(), "unsafe configuration must be rejected");
    } else {
        result.unwrap();
    }
}

fn invoke(home: &Path, action: &str, expect_error: bool) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "agent_hooks::tests::isolated_installer", "--nocapture"])
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("CLAUDE_CONFIG_DIR", home.join("claude"))
        .env("CODEX_HOME", home.join("codex"))
        .env("NOTMUX_HOOK_TEST_ACTION", action)
        .env("NOTMUX_HOOK_TEST_ERROR", if expect_error { "1" } else { "0" })
        .output()
        .unwrap();
    assert!(output.status.success(), "{action}:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}

fn write_json(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn claude_preserves_foreign_hooks_and_settings_through_install_and_uninstall() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("claude/settings.json");
    let original = json!({
        "model": "custom-model",
        "permissions": {"allow": ["Read"]},
        "hooks": {
            "Stop": [{"matcher": "", "hooks": [
                {"type": "command", "command": "echo user-stop", "timeout": 42},
                {"type": "prompt", "prompt": "Keep my prompt hook"}
            ]}],
            "PreToolUse": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": "echo $NOTMUX_SURFACE_ID"}
            ]}],
            "FutureEvent": [{"custom": "keep me"}]
        }
    });
    write_json(&path, &original);
    invoke(home.path(), "install-claude", false);
    let installed = read_json(&path);
    assert_eq!(installed["hooks"]["Stop"][0], original["hooks"]["Stop"][0]);
    assert_eq!(installed["hooks"]["PreToolUse"][0], original["hooks"]["PreToolUse"][0]);
    assert_eq!(installed["hooks"]["FutureEvent"], original["hooks"]["FutureEvent"]);
    let first = std::fs::read(&path).unwrap();
    invoke(home.path(), "install-claude", false);
    assert_eq!(std::fs::read(&path).unwrap(), first, "reinstall must be idempotent");
    invoke(home.path(), "uninstall-claude", false);
    assert_eq!(read_json(&path), original);
}

#[test]
fn claude_rejects_invalid_or_unreadable_settings_without_overwriting() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("claude/settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    for original in ["{broken", "", "[]", "{\"hooks\":false}", "{\"hooks\":{\"Stop\":false}}"] {
        std::fs::write(&path, original).unwrap();
        invoke(home.path(), "install-claude", true);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    invoke(home.path(), "install-claude", true);
    assert!(path.is_dir());
}

#[test]
fn codex_preserves_foreign_hooks_notify_and_trust() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("codex/hooks.json");
    let original = json!({
        "extra": {"keep": true},
        "hooks": {
            "Stop": [{"hooks": [{"type": "command", "command": "echo user-stop", "timeout": 50}]}],
            "SessionStart": [{"matcher": "resume", "hooks": [{"type": "command", "command": "echo user-start"}]}],
            "FutureEvent": [{"custom": "keep me"}]
        }
    });
    write_json(&path, &original);
    let key = std::fs::canonicalize(&path).unwrap().to_string_lossy().to_string();
    let config_path = home.path().join("codex/config.toml");
    let config = format!(
        "# user settings\nnotify = [\"my-notmux-helper\", \"--user\"]\nmodel = 'custom'\n\n[features]\nhooks = true\n\n[hooks.state.\"{}:stop:0:0\"]\ntrusted_hash = \"user-hash\"\n",
        key.replace('\\', "\\\\").replace('"', "\\\"")
    );
    std::fs::write(&config_path, &config).unwrap();
    invoke(home.path(), "install-codex", false);
    let installed = read_json(&path);
    assert_eq!(installed["hooks"]["Stop"][0], original["hooks"]["Stop"][0]);
    assert_eq!(installed["hooks"]["SessionStart"][0], original["hooks"]["SessionStart"][0]);
    assert_eq!(installed["extra"], original["extra"]);
    let installed_config = std::fs::read_to_string(&config_path).unwrap();
    assert!(installed_config.contains("my-notmux-helper"));
    assert!(installed_config.contains("trusted_hash = \"user-hash\""));
    let first = std::fs::read(&path).unwrap();
    invoke(home.path(), "install-codex", false);
    assert_eq!(std::fs::read(&path).unwrap(), first);
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), installed_config);
    invoke(home.path(), "uninstall-codex", false);
    assert_eq!(read_json(&path), original);
    let after = std::fs::read_to_string(&config_path).unwrap();
    assert!(after.contains("my-notmux-helper"));
    assert!(after.contains("trusted_hash = \"user-hash\""));
}

#[test]
fn codex_uninstall_does_not_delete_a_foreign_hooks_file() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("codex/hooks.json");
    let original = json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "echo user"}]}]}});
    write_json(&path, &original);
    let before = std::fs::read(&path).unwrap();
    invoke(home.path(), "uninstall-codex", false);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn codex_validates_both_files_before_modifying_either() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("codex/hooks.json");
    let config_path = home.path().join("codex/config.toml");
    let original = json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "echo user"}]}]}});
    write_json(&path, &original);
    let before = std::fs::read(&path).unwrap();
    std::fs::write(&config_path, "[broken TOML").unwrap();
    invoke(home.path(), "install-codex", true);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), "[broken TOML");
    std::fs::write(&config_path, "model = 'custom'\n").unwrap();
    std::fs::write(&path, "{broken JSON").unwrap();
    invoke(home.path(), "install-codex", true);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{broken JSON");
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), "model = 'custom'\n");
}
