use super::*;
use serde_json::Value;

fn entries(agent_dir: &std::path::Path) -> Vec<Value> {
    let text = std::fs::read_to_string(agent_dir.join("hooks.json")).unwrap();
    match serde_json::from_str::<Value>(&text).unwrap() {
        Value::Array(list) => list,
        Value::Object(mut obj) => obj.remove("hooks").and_then(|h| h.as_array().cloned()).unwrap_or_default(),
        _ => vec![],
    }
}

#[test]
fn install_appends_the_inbox_drain_after_the_status_hooks_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let agent_dir = dir.path().join("agent");
    // A user hook survives.
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("hooks.json"),
        r#"{"other": 1, "hooks": [{"event": "Stop", "command": "say done", "timeout_ms": 5}]}"#,
    )
    .unwrap();

    install_notagent_at(&agent_dir, "/opt/notmux").unwrap();
    let list = entries(&agent_dir);
    assert_eq!(list[0]["command"], "say done", "user hook kept first");
    let ours: Vec<&Value> = list
        .iter()
        .filter(|e| e["command"].as_str().is_some_and(is_notmux_hook_command))
        .collect();
    let last = ours.last().unwrap();
    assert_eq!(last["event"], "UserPromptSubmit");
    assert_eq!(last["timeout_ms"], 15_000);
    let prompt_cmd = last["command"].as_str().unwrap();
    assert!(prompt_cmd.starts_with("[ -n \"$NOTMUX_RUN_ID\" ] && \"/opt/notmux\" agent hook --event prompt"));
    assert!(
        !prompt_cmd.contains(">/dev/null 2>&1") && !prompt_cmd.contains("1>/dev/null"),
        "stdout must reach notagent as context: {prompt_cmd}"
    );
    assert!(prompt_cmd.ends_with("|| true"));
    // Only one orchestration entry, and no Stop hook that could block or
    // continue the agent.
    let orchestration: Vec<&&Value> = ours
        .iter()
        .filter(|e| e["command"].as_str().unwrap().contains("$NOTMUX_RUN_ID"))
        .collect();
    assert_eq!(orchestration.len(), 1);
    // The status hooks are unchanged and precede ours.
    let status: Vec<&Value> = ours[..ours.len() - 1].to_vec();
    assert!(status.iter().all(|e| e["command"].as_str().unwrap().contains("$NOTMUX_SURFACE_ID")));
    assert_eq!(status.len(), 7);
    // Nothing besides hooks.json is written into the agent directory.
    let names: Vec<String> = std::fs::read_dir(&agent_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["hooks.json".to_string()]);

    // Idempotent: a second install leaves hooks.json byte-identical.
    let before = std::fs::read_to_string(agent_dir.join("hooks.json")).unwrap();
    install_notagent_at(&agent_dir, "/opt/notmux").unwrap();
    assert_eq!(std::fs::read_to_string(agent_dir.join("hooks.json")).unwrap(), before);
    assert_eq!(entries(&agent_dir).len(), list.len());
}

#[test]
fn a_rebuilt_binary_replaces_the_previous_orchestration_entry() {
    let dir = tempfile::tempdir().unwrap();
    let agent_dir = dir.path().join("agent");
    install_notagent_at(&agent_dir, "/old/notmux").unwrap();
    install_notagent_at(&agent_dir, "/new/notmux").unwrap();
    let list = entries(&agent_dir);
    assert!(list.iter().all(|e| !e["command"].as_str().unwrap().contains("/old/notmux")));
    assert_eq!(list.iter().filter(|e| e["event"] == "Stop").count(), 1, "only the status Stop hook");
}
