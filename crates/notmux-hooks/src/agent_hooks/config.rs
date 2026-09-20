//! Non-destructive edits to shared agent configuration files.
use serde_json::{Value, json};
use std::fs::{File, OpenOptions, Permissions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

const COMMAND_MARKER: &str = ": notmux-agent-hook-v1; ";

pub(super) struct ConfigFile {
    pub path: PathBuf,
    pub original: Option<String>,
    permissions: Option<Permissions>,
    _lock: File,
}

fn read_optional(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Failed to read {}: {e}; not touching it", path.display())),
    }
}

impl ConfigFile {
    pub fn load(path: &Path) -> Result<Self, String> {
        let parent = path.parent().ok_or("Config path has no parent")?;
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        // Follow an existing symlink rather than replacing the link itself.
        let path = match std::fs::symlink_metadata(path) {
            Ok(_) => std::fs::canonicalize(path),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                std::fs::canonicalize(parent).map(|p| p.join(path.file_name().unwrap()))
            }
            Err(e) => Err(e),
        }.map_err(|e| format!("Failed to resolve {}: {e}", path.display()))?;
        let lock_path = path.with_file_name(format!(
            ".{}.notmux.lock", path.file_name().unwrap().to_string_lossy(),
        ));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(&lock_path)
            .map_err(|e| format!("Failed to open {}: {e}", lock_path.display()))?;
        lock.try_lock().map_err(|e| format!("{} is busy: {e}", path.display()))?;
        let original = read_optional(&path)?;
        let permissions = if original.is_some() {
            Some(std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?.permissions())
        } else {
            None
        };
        Ok(Self { path, original, permissions, _lock: lock })
    }

    pub fn json(&self) -> Result<Value, String> {
        let value = match &self.original {
            Some(text) => serde_json::from_str(text)
                .map_err(|e| format!("{} is not valid JSON ({e}); not touching it", self.path.display()))?,
            None => json!({}),
        };
        if !value.is_object() {
            return Err(format!("{} is not an object; not touching it", self.path.display()));
        }
        Ok(value)
    }

    fn stage(&self, text: &str) -> Result<NamedTempFile, String> {
        if self.permissions.as_ref().is_some_and(Permissions::readonly) {
            return Err(format!("{} is read-only; not touching it", self.path.display()));
        }
        let mut temp = tempfile::Builder::new().prefix(".notmux-hooks-")
            .tempfile_in(self.path.parent().unwrap())
            .map_err(|e| format!("Failed to stage {}: {e}", self.path.display()))?;
        temp.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        if let Some(permissions) = &self.permissions {
            temp.as_file().set_permissions(permissions.clone()).map_err(|e| e.to_string())?;
        }
        temp.as_file().sync_all().map_err(|e| e.to_string())?;
        Ok(temp)
    }
}

/// Stage all files before publishing any. If a later replacement fails, restore
/// earlier files only while they still contain our write, never over a user's edit.
pub(super) fn commit(changes: &[(&ConfigFile, String)]) -> Result<(), String> {
    let mut staged = Vec::new();
    for (file, text) in changes {
        if file.original.as_deref() == Some(text) {
            continue;
        }
        let new = file.stage(text)?;
        let rollback = file.original.as_deref().map(|old| file.stage(old)).transpose()?;
        staged.push((*file, text.as_str(), Some(new), rollback));
    }
    for (file, _, _, _) in &staged {
        if read_optional(&file.path)? != file.original {
            return Err(format!("{} changed during installation; not touching it", file.path.display()));
        }
    }
    for i in 0..staged.len() {
        let (file, _, temp, _) = &mut staged[i];
        let published = match read_optional(&file.path) {
            Ok(current) if current == file.original => temp.take().unwrap()
                .persist(&file.path).map(|_| ())
                .map_err(|e| format!("Failed to replace {}: {e}", file.path.display())),
            Ok(_) => Err(format!("{} changed before publication", file.path.display())),
            Err(error) => Err(error),
        };
        if let Err(mut message) = published {
            for (file, written, _, rollback) in staged[..i].iter_mut().rev() {
                if read_optional(&file.path).ok().flatten().as_deref() != Some(*written) {
                    message.push_str(&format!("; {} changed before rollback", file.path.display()));
                    if let Some(backup) = rollback.take() {
                        if let Ok((_, path)) = backup.keep() {
                            message.push_str(&format!("; original saved at {}", path.display()));
                        }
                    }
                    continue;
                }
                if let Some(backup) = rollback.take() {
                    if let Err(error) = backup.persist(&file.path) {
                        if let Ok((_, path)) = error.file.keep() {
                            message.push_str(&format!("; rollback failed; original saved at {}", path.display()));
                        }
                    }
                } else if let Err(error) = std::fs::remove_file(&file.path) {
                    message.push_str(&format!("; rollback failed: {error}"));
                }
            }
            return Err(message);
        }
    }
    Ok(())
}

// Old generated bodies are recognized exactly, allowing only the executable
// path to differ. Mentioning notmux or its environment is not ownership proof.
fn normalized_command(command: &str) -> String {
    let current_exe = super::notmux_binary();
    command.split('"').enumerate().map(|(index, part)| {
        let basename = part.rsplit(['/', '\\']).next().unwrap_or(part);
        if index % 2 == 1 && (part == current_exe || matches!(basename, "notmux" | "notmux.exe")) {
            "<notmux-binary>"
        } else {
            part
        }
    }).collect::<Vec<_>>().join("\"")
}

fn owned_hook(hook: &Value, templates: &[String]) -> bool {
    if hook.get("type").and_then(Value::as_str) != Some("command") {
        return false;
    }
    let Some(command) = hook.get("command").and_then(Value::as_str) else { return false };
    command.starts_with(COMMAND_MARKER) || templates.contains(&normalized_command(command))
}

fn event_label(event: &str) -> String {
    let mut out = String::new();
    for c in event.chars() {
        if c.is_ascii_uppercase() && !out.is_empty() { out.push('_'); }
        out.push(c.to_ascii_lowercase());
    }
    out
}

#[derive(Default)]
pub(super) struct HookEdits {
    // Position suffixes in the agent's trust keys. None means our entry was removed.
    moves: Vec<(String, Option<String>)>,
    installed: Vec<(String, String)>,
}

/// Preserve every foreign group/handler and its metadata. New owned handlers
/// live in separate groups; no foreign command is included in our trust hash.
pub(super) fn merge_hooks(doc: &mut Value, templates: &Value, install: bool) -> Result<HookEdits, String> {
    let root = doc.as_object_mut().ok_or("Hook config is not an object")?;
    if !root.contains_key("hooks") && !install { return Ok(HookEdits::default()); }
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().ok_or("hooks is not an object; not touching it")?;
    let mut edits = HookEdits::default();
    for (event, template_groups) in templates["hooks"].as_object().unwrap() {
        let template_groups = template_groups.as_array().unwrap();
        let commands: Vec<_> = template_groups.iter()
            .flat_map(|g| g["hooks"].as_array().unwrap())
            .filter_map(|h| h["command"].as_str()).map(normalized_command).collect();
        let label = event_label(event);
        let Some(groups) = hooks.get(event).cloned().or_else(|| install.then(|| json!([]))) else { continue };
        let groups = groups.as_array().ok_or_else(|| format!("hooks.{event} is not an array; not touching it"))?;
        let mut kept = Vec::new();
        for (group_index, group) in groups.iter().enumerate() {
            let handlers = group.get("hooks").and_then(Value::as_array)
                .ok_or_else(|| format!("hooks.{event}[{group_index}].hooks is not an array; not touching it"))?;
            let mut remaining = Vec::new();
            let mut removed = false;
            for (hook_index, handler) in handlers.iter().enumerate() {
                let old = format!("{label}:{group_index}:{hook_index}");
                if owned_hook(handler, &commands) {
                    edits.moves.push((old, None));
                    removed = true;
                } else {
                    let new = format!("{label}:{}:{}", kept.len(), remaining.len());
                    if old != new { edits.moves.push((old, Some(new))); }
                    remaining.push(handler.clone());
                }
            }
            if removed && remaining.is_empty()
                && group.as_object().unwrap().keys().all(|k| k == "hooks" || k == "matcher") {
                continue;
            }
            let mut group = group.clone();
            group["hooks"] = Value::Array(remaining);
            kept.push(group);
        }
        if install {
            for group in template_groups {
                let mut group = group.clone();
                for (hook_index, hook) in group["hooks"].as_array_mut().unwrap().iter_mut().enumerate() {
                    let command = format!("{COMMAND_MARKER}{}", hook["command"].as_str().unwrap());
                    hook["command"] = Value::String(command.clone());
                    let suffix = format!("{label}:{}:{hook_index}", kept.len());
                    let hash = super::codex_hook_trust_hash(&label, &command,
                        hook["timeout"].as_u64().unwrap_or(super::CODEX_HOOK_TIMEOUT_MS));
                    edits.installed.push((suffix, hash));
                }
                kept.push(group);
            }
        }
        if kept.is_empty() && !groups.is_empty() {
            hooks.remove(event);
        } else if install || !groups.is_empty() {
            hooks.insert(event.clone(), Value::Array(kept));
        }
    }
    if hooks.is_empty() && !edits.moves.is_empty() { root.remove("hooks"); }
    Ok(edits)
}

fn table<'a>(parent: &'a mut dyn toml_edit::TableLike, key: &str) -> Result<&'a mut dyn toml_edit::TableLike, String> {
    if !parent.contains_key(key) { parent.insert(key, toml_edit::table()); }
    parent.get_mut(key).unwrap().as_table_like_mut()
        .ok_or_else(|| format!("{key} is not a TOML table; not touching it"))
}

pub(super) fn update_codex_config(
    doc: &mut toml_edit::DocumentMut, hooks_path: &Path, edits: &HookEdits, install: bool,
) -> Result<(), String> {
    if install {
        let features = table(doc.as_table_mut(), "features")?;
        if !features.contains_key("hooks") {
            features.insert("hooks", toml_edit::value(true));
        } else if features.get("hooks").and_then(toml_edit::Item::as_bool).is_none() {
            return Err("features.hooks is not a boolean; not touching it".into());
        }
    }
    if edits.moves.is_empty() && edits.installed.is_empty() { return Ok(()); }
    // Uninstall never creates a trust table when none existed.
    if !install && doc.get("hooks").and_then(|h| h.get("state")).is_none() { return Ok(()); }
    let state = table(table(doc.as_table_mut(), "hooks")?, "state")?;
    let prefix = hooks_path.to_string_lossy();
    let mut moved = Vec::new();
    let mut owned_trust = std::collections::HashMap::new();
    for (old, new) in &edits.moves {
        if let Some(value) = state.remove(&format!("{prefix}:{old}")) {
            if let Some(new) = new {
                moved.push((format!("{prefix}:{new}"), value));
            } else {
                owned_trust.entry(old.split(':').next().unwrap()).or_insert(value);
            }
        }
    }
    for (key, value) in moved {
        if state.contains_key(&key) {
            return Err(format!("Hook trust key collision at {key}; not touching it"));
        }
        state.insert(&key, value);
    }
    for (suffix, hash) in &edits.installed {
        let key = format!("{prefix}:{suffix}");
        if state.contains_key(&key) {
            return Err(format!("Unowned hook trust key at {key}; not touching it"));
        }
        let mut entry = owned_trust.remove(suffix.split(':').next().unwrap())
            .unwrap_or_else(toml_edit::table);
        entry.as_table_like_mut().ok_or("Hook trust record is not a table; not touching it")?
            .insert("trusted_hash", toml_edit::value(hash));
        state.insert(&key, entry);
    }
    Ok(())
}

pub(super) fn edit_claude(path: &Path, templates: Value, install: bool) -> Result<(), String> {
    let file = ConfigFile::load(path)?;
    let original = file.json()?;
    let mut doc = original.clone();
    merge_hooks(&mut doc, &templates, install)?;
    if doc != original {
        commit(&[(&file, serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?)])?;
    }
    Ok(())
}

pub(super) fn edit_codex(directory: &Path, templates: Value, install: bool) -> Result<(), String> {
    let hooks = ConfigFile::load(&directory.join("hooks.json"))?;
    let config = ConfigFile::load(&directory.join("config.toml"))?;
    let original = hooks.json()?;
    let mut doc = original.clone();
    let mut toml = config.original.as_deref().unwrap_or("").parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("{} is not valid TOML ({e}); not touching it", config.path.display()))?;
    let edits = merge_hooks(&mut doc, &templates, install)?;
    if !install && doc == original { return Ok(()); }
    update_codex_config(&mut toml, &hooks.path, &edits, install)?;
    let mut changes = Vec::new();
    if doc != original {
        changes.push((&hooks, serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?));
    }
    let updated_config = toml.to_string();
    if updated_config != config.original.as_deref().unwrap_or("") {
        changes.push((&config, updated_config));
    }
    commit(&changes)
}
