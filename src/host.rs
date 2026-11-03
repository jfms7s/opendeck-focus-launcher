//! Running a program on the host. Inside a Flatpak sandbox (OpenDeck
//! installed as a Flatpak) every host program, whether an app launch, a
//! window tool (kdotool, wmctrl, xdotool) or `gsettings`, is run through
//! `flatpak-spawn --host`; outside one it runs directly. Every subprocess
//! that touches the host is built here, so the two modes can't drift.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Whether the plugin runs inside a Flatpak sandbox (detected the same way
/// the Linux App Launcher plugin, `oadesktopentry`, does).
pub fn sandboxed() -> bool {
    std::env::var_os("FLATPAK_ID").is_some()
}

/// What the host program is for, which decides how it is wrapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostUse {
    /// A short call whose output the plugin waits for. `--watch-bus` makes
    /// the host process exit when `flatpak-spawn` does, so a call killed on
    /// timeout doesn't leave the host tool running.
    Tool,
    /// An app launch: it must outlive the call (and the plugin), so it is
    /// not tied to `flatpak-spawn`'s lifetime.
    App,
}

/// The program and arguments actually executed to run `program args...` on
/// the host: unchanged outside a sandbox, else behind `flatpak-spawn --host`.
pub fn host_argv(
    program: &str,
    args: &[OsString],
    sandboxed: bool,
    host_use: HostUse,
) -> (String, Vec<OsString>) {
    if !sandboxed {
        return (program.to_string(), args.to_vec());
    }
    let mut wrapped: Vec<OsString> = vec!["--host".into()];
    if host_use == HostUse::Tool {
        wrapped.push("--watch-bus".into());
    }
    wrapped.push(program.into());
    wrapped.extend_from_slice(args);
    ("flatpak-spawn".to_string(), wrapped)
}

fn os_args<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Vec<OsString> {
    args.iter().map(|a| a.as_ref().to_os_string()).collect()
}

/// A `tokio` command for `program args...` on the host.
pub fn command<S: AsRef<std::ffi::OsStr>>(
    program: &str,
    args: &[S],
    host_use: HostUse,
) -> tokio::process::Command {
    let (program, args) = host_argv(program, &os_args(args), sandboxed(), host_use);
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    cmd
}

/// A blocking `std` command for `program args...` on the host (a tool call).
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn std_command<S: AsRef<std::ffi::OsStr>>(program: &str, args: &[S]) -> std::process::Command {
    let (program, args) = host_argv(program, &os_args(args), sandboxed(), HostUse::Tool);
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    cmd
}

/// `flatpak-spawn` reports a host program it could not start on stderr
/// (with a non-zero exit) rather than as a spawn error: is this that, for a
/// program that isn't installed on the host?
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn host_program_missing(stderr: &str) -> bool {
    stderr.contains("Failed to start command") && stderr.contains("No such file or directory")
}

/// The host's application directories as seen from inside the sandbox, in
/// XDG precedence order (user before system), for app discovery. Which of
/// them exist depends on OpenDeck's Flatpak filesystem permissions; missing
/// ones are skipped by the caller.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn host_application_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".local/share/applications"),
        home.join(".local/share/flatpak/exports/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
        PathBuf::from("/run/host/usr/local/share/applications"),
        PathBuf::from("/run/host/usr/share/applications"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(args: &[OsString]) -> Vec<&str> {
        args.iter().map(|a| a.to_str().unwrap()).collect()
    }

    #[test]
    fn outside_a_sandbox_the_program_runs_directly() {
        for host_use in [HostUse::Tool, HostUse::App] {
            let (program, args) = host_argv(
                "kdotool",
                &["search".into(), "--class".into()],
                false,
                host_use,
            );
            assert_eq!(program, "kdotool");
            assert_eq!(strs(&args), ["search", "--class"]);
        }
    }

    #[test]
    fn a_sandboxed_tool_runs_on_the_host_and_dies_with_flatpak_spawn() {
        let (program, args) = host_argv(
            "kdotool",
            &["windowactivate".into(), "{abc}".into()],
            true,
            HostUse::Tool,
        );
        assert_eq!(program, "flatpak-spawn");
        assert_eq!(
            strs(&args),
            [
                "--host",
                "--watch-bus",
                "kdotool",
                "windowactivate",
                "{abc}"
            ]
        );
    }

    #[test]
    fn a_sandboxed_app_runs_on_the_host_without_a_shell_and_outlives_the_call() {
        let (program, args) = host_argv(
            "firefox",
            &["--new-window".into(), "a b; rm -rf ~".into()],
            true,
            HostUse::App,
        );
        assert_eq!(program, "flatpak-spawn");
        assert_eq!(
            strs(&args),
            ["--host", "firefox", "--new-window", "a b; rm -rf ~"]
        );
    }

    #[test]
    fn std_and_tokio_commands_use_the_same_argv() {
        if sandboxed() {
            return; // the wrapped shape is covered by the host_argv tests
        }
        let cmd = std_command("gsettings", &["get", "a", "b"]);
        assert_eq!(cmd.get_program(), "gsettings");
        let cmd = command("wmctrl", &["-l", "-x"], HostUse::Tool);
        assert_eq!(cmd.as_std().get_program(), "wmctrl");
        assert_eq!(cmd.as_std().get_args().collect::<Vec<_>>(), ["-l", "-x"]);
    }

    #[test]
    fn recognises_flatpak_spawns_missing_program_message() {
        assert!(host_program_missing(
            "Failed to start command: Failed to execute child process \u{201c}kdotool\u{201d} (No such file or directory)\n"
        ));
        assert!(!host_program_missing(
            "Failed to start command: Permission denied"
        ));
        assert!(!host_program_missing("window not found"));
    }

    #[test]
    fn host_application_dirs_put_user_dirs_first() {
        let dirs = host_application_dirs(Path::new("/home/u"));
        assert_eq!(dirs[0], Path::new("/home/u/.local/share/applications"));
        assert_eq!(
            dirs.last().unwrap(),
            Path::new("/run/host/usr/share/applications")
        );
    }
}
