use crate::shell_config::ShellType;
use portable_pty::CommandBuilder;
use std::path::{Path, PathBuf};

#[cfg(unix)]
pub fn build_command(cwd: &str, shell: Option<&ShellType>) -> Option<CommandBuilder> {
    let shell_path = match shell {
        Some(ShellType::Default) | None => std::env::var("SHELL").ok()?,
        Some(ShellType::Custom { path, args }) if args.is_empty() => path.clone(),
        Some(_) => return None,
    };

    let shell_kind = ShellKind::from_path(&shell_path)?;
    let dir = integration_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;

    let mut cmd = match shell_kind {
        ShellKind::Zsh => {
            let rc_dir = dir.join("zsh");
            std::fs::create_dir_all(&rc_dir).ok()?;
            std::fs::write(rc_dir.join(".zshrc"), zsh_script()).ok()?;

            let original_zdotdir = std::env::var("ZDOTDIR")
                .ok()
                .or_else(|| std::env::var("HOME").ok())
                .unwrap_or_default();
            let mut cmd = CommandBuilder::new(&shell_path);
            cmd.env("ZDOTDIR", &rc_dir);
            cmd.env("VRYN_ORIGINAL_ZDOTDIR", original_zdotdir);
            cmd
        }
        ShellKind::Bash => {
            let rcfile = dir.join("bashrc");
            std::fs::write(&rcfile, bash_script()).ok()?;

            let original_bashrc = std::env::var("HOME")
                .ok()
                .map(|home| Path::new(&home).join(".bashrc"))
                .unwrap_or_default();
            let mut cmd = CommandBuilder::new(&shell_path);
            cmd.arg("--rcfile");
            cmd.arg(&rcfile);
            cmd.arg("-i");
            cmd.env("VRYN_ORIGINAL_BASHRC", original_bashrc);
            cmd
        }
        ShellKind::Fish => {
            let script = dir.join("fish-init.fish");
            std::fs::write(&script, fish_script()).ok()?;

            let mut cmd = CommandBuilder::new(&shell_path);
            cmd.arg("--init-command");
            cmd.arg(format!("source {}", fish_quote(&script.to_string_lossy())));
            cmd
        }
    };

    cmd.cwd(cwd);
    Some(cmd)
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShellKind {
    Bash,
    Zsh,
    Fish,
}

#[cfg(unix)]
impl ShellKind {
    fn from_path(path: &str) -> Option<Self> {
        let name = Path::new(path).file_name()?.to_string_lossy();
        let name = name.strip_prefix('-').unwrap_or(&name);
        if name == "bash" {
            Some(Self::Bash)
        } else if name == "zsh" {
            Some(Self::Zsh)
        } else if name == "fish" {
            Some(Self::Fish)
        } else {
            None
        }
    }
}

#[cfg(unix)]
fn integration_dir() -> std::io::Result<PathBuf> {
    let base = std::env::temp_dir().join("vryn-terminal-integration");
    let user = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
    Ok(base.join(user))
}

#[cfg(unix)]
fn zsh_script() -> &'static str {
    r#"
if [[ -n "${VRYN_ORIGINAL_ZDOTDIR:-}" && -r "${VRYN_ORIGINAL_ZDOTDIR}/.zshrc" ]]; then
  source "${VRYN_ORIGINAL_ZDOTDIR}/.zshrc"
fi

__vryn_osc() {
  builtin printf '\033]777;vryn;%s;%s\a' "$1" "$2"
}

__vryn_prompt_start() {
  __vryn_osc A "$PWD"
}

__vryn_preexec() {
  __VRYN_COMMAND_RUNNING=1
  __vryn_osc B "$1"
  __vryn_osc C ""
}

__vryn_precmd() {
  local exit_code="$?"
  if [[ -n "${__VRYN_COMMAND_RUNNING:-}" ]]; then
    __vryn_osc D "$exit_code"
    unset __VRYN_COMMAND_RUNNING
  fi
  __vryn_prompt_start
}

autoload -Uz add-zsh-hook
add-zsh-hook preexec __vryn_preexec
add-zsh-hook precmd __vryn_precmd
"#
}

#[cfg(unix)]
fn bash_script() -> &'static str {
    r#"
if [[ -n "${VRYN_ORIGINAL_BASHRC:-}" && -r "${VRYN_ORIGINAL_BASHRC}" ]]; then
  source "${VRYN_ORIGINAL_BASHRC}"
fi

__vryn_osc() {
  printf '\033]777;vryn;%s;%s\a' "$1" "$2"
}

__vryn_last_command="$(fc -ln -1 2>/dev/null || true)"
__vryn_original_prompt_command="${PROMPT_COMMAND:-}"

__vryn_prompt_command() {
  local status="$?"
  local command
  command="$(fc -ln -1 2>/dev/null || true)"
  if [[ -n "${command//[[:space:]]/}" && "$command" != "$__vryn_last_command" ]]; then
    __vryn_osc B "$command"
    __vryn_osc C ""
    __vryn_osc D "$status"
    __vryn_last_command="$command"
  fi
  __vryn_osc A "$PWD"
  if [[ -n "$__vryn_original_prompt_command" ]]; then
    eval "$__vryn_original_prompt_command"
  fi
}

PROMPT_COMMAND=__vryn_prompt_command
"#
}

#[cfg(unix)]
fn fish_script() -> &'static str {
    r#"
function __vryn_osc
    printf '\e]777;vryn;%s;%s\a' $argv[1] $argv[2]
end

function __vryn_preexec --on-event fish_preexec
    set -g __vryn_command_running 1
    __vryn_osc B "$argv"
    __vryn_osc C ""
end

function __vryn_postexec --on-event fish_postexec
    set -l code $status
    if set -q __vryn_command_running
        __vryn_osc D "$code"
        set -e __vryn_command_running
    end
    __vryn_osc A "$PWD"
end

__vryn_osc A "$PWD"
"#
}

#[cfg(unix)]
fn fish_quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn detects_supported_shells() {
        assert_eq!(ShellKind::from_path("/bin/bash"), Some(ShellKind::Bash));
        assert_eq!(ShellKind::from_path("/usr/bin/zsh"), Some(ShellKind::Zsh));
        assert_eq!(ShellKind::from_path("/opt/homebrew/bin/fish"), Some(ShellKind::Fish));
        assert_eq!(ShellKind::from_path("/bin/sh"), None);
    }

    #[cfg(unix)]
    #[test]
    fn builds_bash_command_with_rcfile() {
        let shell = ShellType::Custom {
            path: "/bin/bash".to_string(),
            args: Vec::new(),
        };
        let cmd = build_command("/tmp", Some(&shell)).expect("bash command");
        let argv: Vec<_> = cmd
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();

        assert_eq!(argv[0], "/bin/bash");
        assert!(argv.iter().any(|arg| arg == "--rcfile"));
        assert!(argv.iter().any(|arg| arg == "-i"));
        assert_eq!(cmd.get_cwd().and_then(|cwd| cwd.to_str()), Some("/tmp"));
    }

    #[cfg(unix)]
    #[test]
    fn zsh_script_does_not_assign_readonly_status_variable() {
        assert!(!zsh_script().contains("local status="));
        assert!(zsh_script().contains("local exit_code="));
    }
}
