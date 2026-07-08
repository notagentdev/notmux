use notmux_hooks::agent_hooks;

pub fn cli_hooks(args: &[String]) -> i32 {
    let pos: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(|s| s.as_str())
        .collect();
    let action = pos.first().copied().unwrap_or("setup");
    let agent = pos.get(1).copied();

    let result = match (action, agent) {
        ("setup", None) | ("install", None) => {
            let errors = agent_hooks::install_all();
            if errors.is_empty() {
                println!("All hooks installed. Agents will now notify notmux when they finish.");
                Ok(())
            } else {
                Err(errors.join("\n"))
            }
        }
        ("setup", Some("claude")) | ("install", Some("claude")) => agent_hooks::install_claude(),
        ("setup", Some("codex")) | ("install", Some("codex")) => agent_hooks::install_codex(),
        ("setup", Some("shell")) | ("install", Some("shell")) => agent_hooks::install_shell(),
        ("uninstall", None) => {
            let errors = agent_hooks::uninstall_all();
            if errors.is_empty() {
                println!("All hooks removed.");
                Ok(())
            } else {
                Err(errors.join("\n"))
            }
        }
        ("uninstall", Some("claude")) => agent_hooks::uninstall_claude(),
        ("uninstall", Some("codex")) => agent_hooks::uninstall_codex(),
        ("uninstall", Some("shell")) => agent_hooks::uninstall_shell(),
        ("list", _) => {
            println!("Available agents: claude, codex, shell");
            println!("Usage: notmux hooks setup [agent]");
            println!("       notmux hooks uninstall [agent]");
            Ok(())
        }
        _ => Err(format!(
            "Unknown hooks command: {action} {agent:?}\nUsage: notmux hooks setup [claude|codex|shell]"
        )),
    };

    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}
