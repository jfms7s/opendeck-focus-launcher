//! Installed-app discovery (`.desktop` files) and launching.
//!
//! Launching never goes through a shell. `Exec=` lines are tokenized with
//! the Desktop Entry quoting rules, user-typed overrides with shell-style
//! quoting only (no expansion), and the resulting argv is spawned directly.

#[cfg(not(target_os = "macos"))]
use freedesktop_desktop_entry::{DesktopEntry, Iter, default_paths};
#[cfg(not(target_os = "macos"))]
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppEntry {
    pub id: String,
    pub name: String,
    /// `StartupWMClass=`, else the desktop id (see `resolve_window_class`).
    pub window_class: String,
    /// The raw `Exec=` value, tokenized only at launch time.
    pub exec: String,
    /// The raw `Icon=` value: a theme icon name or an absolute path.
    pub icon: Option<String>,
    /// The `.desktop` file's own path, shown read-only in the property
    /// inspector and substituted for `%k`.
    pub path: PathBuf,
}

/// The window class to search for: an explicit `StartupWMClass` if the entry
/// declares one, else the desktop file's own id (the common fallback for
/// entries that don't set `StartupWMClass`).
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn resolve_window_class(entry_id: &str, startup_wm_class: Option<&str>) -> String {
    startup_wm_class
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .unwrap_or(entry_id)
        .to_string()
}

/// Installed `.app` bundles (see `bundle.rs`).
#[cfg(target_os = "macos")]
pub fn list_installed_apps() -> Vec<AppEntry> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    crate::bundle::list_apps_in(&crate::bundle::search_dirs(&home))
}

#[cfg(not(target_os = "macos"))]
pub fn list_installed_apps() -> Vec<AppEntry> {
    let desktops = current_desktops(std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref());
    list_installed_apps_from_paths(default_paths(), &desktops)
}

/// The colon-separated `XDG_CURRENT_DESKTOP` list, blanks dropped.
#[cfg(not(target_os = "macos"))]
fn current_desktops(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(':')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether an entry's `OnlyShowIn=`/`NotShowIn=` let it show on the current
/// desktops (ASCII case ignored). With no current desktop known, everything
/// shows.
#[cfg(not(target_os = "macos"))]
fn shown_on(only: Option<Vec<&str>>, not: Option<Vec<&str>>, desktops: &[String]) -> bool {
    if desktops.is_empty() {
        return true;
    }
    let on_current = |list: &[&str]| {
        list.iter()
            .any(|item| desktops.iter().any(|d| d.eq_ignore_ascii_case(item.trim())))
    };
    if not.is_some_and(|list| on_current(&list)) {
        return false;
    }
    only.is_none_or(|list| on_current(&list))
}

#[cfg(not(target_os = "macos"))]
fn list_installed_apps_from_paths<I: IntoIterator<Item = PathBuf>>(
    paths: I,
    desktops: &[String],
) -> Vec<AppEntry> {
    let locales = freedesktop_desktop_entry::get_languages_from_env();
    // XDG data dirs can list the same app id twice (e.g. a Flatpak override
    // in ~/.local/share/applications shadowing the system copy).
    // `default_paths()`/`Iter` walk user dirs before system dirs, i.e. in XDG
    // precedence order, so the first occurrence of each id wins. The id is
    // recorded before any visibility check, so a hidden higher-precedence
    // copy (`Hidden=true`, the spec's "deleted", a `NoDisplay=true` override
    // in ~/.local/share/applications, or one meant for another desktop)
    // hides the lower-precedence copies too.
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut apps: Vec<AppEntry> = Iter::new(paths.into_iter())
        .entries(Some(locales.as_slice()))
        .filter(|entry: &DesktopEntry| seen_ids.insert(entry.id().to_string()))
        .filter(|entry| {
            // `Type=Link`/`Type=Directory` entries have no `Exec=`, so
            // surfacing them in the dropdown would silently no-op on select.
            !entry.no_display()
                && !entry.hidden()
                && entry.type_() == Some("Application")
                && shown_on(entry.only_show_in(), entry.not_show_in(), desktops)
        })
        .map(|entry| AppEntry {
            id: entry.id().to_string(),
            name: entry
                .name(locales.as_slice())
                .map_or_else(|| entry.id().to_string(), |n| n.to_string()),
            window_class: resolve_window_class(entry.id(), entry.startup_wm_class()),
            exec: entry.exec().unwrap_or_default().to_string(),
            icon: entry
                .icon()
                .map(str::trim)
                .filter(|i| !i.is_empty())
                .map(str::to_string),
            path: entry.path.clone(),
        })
        .collect();
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExecError {
    #[error("the launch command is empty")]
    Empty,
    #[error("unterminated quote in the launch command")]
    UnterminatedQuote,
    #[error("could not split the arguments: {0}")]
    BadArguments(String),
    /// The first word is empty or looks like an option (`-…`), which
    /// `flatpak-spawn --host` would parse as one of its own options.
    #[error("the command must start with a program, not {0:?}")]
    NotAProgram(String),
}

/// What the `%i`, `%c` and `%k` field codes expand to.
pub struct ExecContext<'a> {
    pub icon: Option<&'a str>,
    pub name: &'a str,
    pub desktop_file: &'a Path,
}

impl<'a> ExecContext<'a> {
    pub fn for_entry(entry: &'a AppEntry) -> Self {
        Self {
            icon: entry.icon.as_deref(),
            name: &entry.name,
            desktop_file: &entry.path,
        }
    }
}

/// Undoes the Desktop Entry *string* escapes (`\s`, `\n`, `\t`, `\r`,
/// `\\`). Other backslashes are kept for the quoting pass that follows.
fn unescape_string_value(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            _ => {
                out.push('\\');
                continue;
            }
        }
        chars.next();
    }
    out
}

struct RawArg {
    text: String,
    quoted: bool,
}

/// Splits an `Exec=` value per the Desktop Entry spec: whitespace separates
/// arguments; double quotes group, and inside them `\"`, `` \` ``, `\$` and
/// `\\` stand for the literal character.
fn split_exec(value: &str) -> Result<Vec<RawArg>, ExecError> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_arg = false;
    let mut quoted = false;
    let mut in_quotes = false;
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' => in_quotes = false,
                '\\' if matches!(chars.peek(), Some('"' | '`' | '$' | '\\')) => {
                    current.extend(chars.next());
                }
                _ => current.push(c),
            }
        } else if c.is_whitespace() {
            if in_arg {
                args.push(RawArg {
                    text: std::mem::take(&mut current),
                    quoted,
                });
                in_arg = false;
                quoted = false;
            }
        } else if c == '"' {
            in_quotes = true;
            in_arg = true;
            quoted = true;
        } else {
            current.push(c);
            in_arg = true;
        }
    }
    if in_quotes {
        return Err(ExecError::UnterminatedQuote);
    }
    if in_arg {
        args.push(RawArg {
            text: current,
            quoted,
        });
    }
    Ok(args)
}

/// Expands one unquoted argument's field codes. No files or URLs are ever
/// passed, so `%f %F %u %U` vanish; `%i` becomes `--icon <Icon>`, `%c` the
/// name and `%k` the `.desktop` path, as the spec says; deprecated codes
/// vanish; `%%` is a literal `%`. Codes embedded mid-argument (not valid per
/// the spec) are dropped from the argument, keeping the rest of it.
fn expand_field_codes(arg: &str, ctx: &ExecContext) -> Vec<String> {
    match arg {
        "%f" | "%F" | "%u" | "%U" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => Vec::new(),
        "%i" => ctx
            .icon
            .map(|icon| vec!["--icon".to_string(), icon.to_string()])
            .unwrap_or_default(),
        "%c" => vec![ctx.name.to_string()],
        "%k" => vec![ctx.desktop_file.to_string_lossy().into_owned()],
        _ => {
            let mut out = String::with_capacity(arg.len());
            let mut chars = arg.chars().peekable();
            while let Some(c) = chars.next() {
                if c != '%' {
                    out.push(c);
                    continue;
                }
                match chars.peek() {
                    Some('%') => {
                        out.push('%');
                        chars.next();
                    }
                    Some(code) if code.is_ascii_alphabetic() => {
                        chars.next();
                    }
                    _ => out.push('%'),
                }
            }
            if out.is_empty() {
                Vec::new()
            } else {
                vec![out]
            }
        }
    }
}

// freedesktop-desktop-entry's DesktopEntry::parse_exec mis-splits quoted
// arguments (https://github.com/pop-os/freedesktop-desktop-entry/issues/36,
// open as of 0.8.4). Re-check when Dependabot proposes a newer version; if
// it's fixed and the three real_* tests below pass with it, this tokenizer
// can go.
/// Turns a `.desktop` `Exec=` value into an argv.
pub fn parse_desktop_exec(raw: &str, ctx: &ExecContext) -> Result<Vec<String>, ExecError> {
    let argv: Vec<String> = split_exec(&unescape_string_value(raw))?
        .into_iter()
        .flat_map(|arg| {
            if arg.quoted {
                vec![arg.text]
            } else {
                expand_field_codes(&arg.text, ctx)
            }
        })
        .collect();
    if argv.is_empty() {
        return Err(ExecError::Empty);
    }
    Ok(argv)
}

/// Splits a user-typed command or argument list with shell-style quoting
/// (single quotes, double quotes, backslashes) but no expansion of any kind:
/// `$(...)`, backticks, `;`, `|` and globs are plain text. Standalone field
/// codes (pasted from an `Exec=` line) are dropped.
pub fn split_user_command(raw: &str) -> Result<Vec<String>, ExecError> {
    let words = shell_words::split(raw).map_err(|e| ExecError::BadArguments(e.to_string()))?;
    Ok(words
        .into_iter()
        .filter(|w| {
            !matches!(
                w.as_str(),
                "%f" | "%F"
                    | "%u"
                    | "%U"
                    | "%i"
                    | "%c"
                    | "%k"
                    | "%d"
                    | "%D"
                    | "%n"
                    | "%N"
                    | "%v"
                    | "%m"
            )
        })
        .collect())
}

/// The argv to launch: `exec_override` (user-typed) if given, else the
/// entry's `Exec=`, followed by any `custom_args`.
pub fn build_launch_argv(
    entry: &AppEntry,
    exec_override: Option<&str>,
    custom_args: Option<&str>,
) -> Result<Vec<String>, ExecError> {
    build_launch_argv_on(cfg!(target_os = "macos"), entry, exec_override, custom_args)
}

/// On macOS an app (no override) is opened by bundle id, `open -b <id>`,
/// and its custom arguments go after `--args`; elsewhere the entry's
/// `Exec=` line is used. An override runs as written on both.
fn build_launch_argv_on(
    macos: bool,
    entry: &AppEntry,
    exec_override: Option<&str>,
    custom_args: Option<&str>,
) -> Result<Vec<String>, ExecError> {
    let (mut argv, extra_marker) = match exec_override {
        Some(cmd) => (split_user_command(cmd)?, None),
        None if macos && entry.id.trim().is_empty() => return Err(ExecError::Empty),
        None if macos => (
            vec!["open".to_string(), "-b".to_string(), entry.id.clone()],
            Some("--args"),
        ),
        None => (
            parse_desktop_exec(&entry.exec, &ExecContext::for_entry(entry))?,
            None,
        ),
    };
    match argv.first() {
        None => return Err(ExecError::Empty),
        Some(program) if program.is_empty() || program.starts_with('-') => {
            return Err(ExecError::NotAProgram(program.clone()));
        }
        Some(_) => {}
    }
    if let Some(extra) = custom_args {
        let extra = split_user_command(extra)?;
        if !extra.is_empty() {
            argv.extend(extra_marker.map(str::to_string));
            argv.extend(extra);
        }
    }
    Ok(argv)
}

/// Starts a process from an argv. A trait so orchestration tests can record
/// launches instead of spawning anything.
pub trait Launcher: Send + Sync {
    fn launch(&self, argv: &[String]) -> std::io::Result<()>;
}

/// Spawns directly, or through `flatpak-spawn --host` when the plugin runs
/// inside a Flatpak sandbox (detected the same way `oadesktopentry` does).
pub struct SystemLauncher;

/// The program and arguments actually executed for `argv`.
fn host_argv(argv: &[String], sandboxed: bool) -> Vec<String> {
    if sandboxed {
        let mut wrapped = vec!["flatpak-spawn".to_string(), "--host".to_string()];
        wrapped.extend_from_slice(argv);
        wrapped
    } else {
        argv.to_vec()
    }
}

impl Launcher for SystemLauncher {
    fn launch(&self, argv: &[String]) -> std::io::Result<()> {
        let full = host_argv(argv, std::env::var_os("FLATPAK_ID").is_some());
        let (program, rest) = full
            .split_first()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty argv"))?;
        let mut child = tokio::process::Command::new(program)
            .args(rest)
            // The app's own output stays out of the plugin log, which may
            // be shared in bug reports; a failing exit is still logged below.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| std::io::Error::new(e.kind(), format!("{program}: {e}")))?;
        // Reap the child when it exits instead of leaving that to tokio's
        // orphan reaper, and log a failing exit (e.g. `flatpak-spawn` not
        // finding the host binary) since nothing else would report it.
        let program = program.clone();
        tokio::spawn(async move {
            match child.wait().await {
                Ok(status) if !status.success() => log::warn!("{program} exited with {status}"),
                Ok(_) => {}
                Err(e) => log::warn!("could not wait for {program}: {e}"),
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(id: &str) -> AppEntry {
        AppEntry {
            id: id.to_string(),
            name: "Safari".to_string(),
            window_class: id.to_string(),
            exec: format!("open -b {id}"),
            icon: None,
            path: PathBuf::from("/Applications/Safari.app"),
        }
    }

    #[test]
    fn macos_launches_by_bundle_id() {
        let argv = build_launch_argv_on(true, &bundle("com.apple.Safari"), None, None).unwrap();
        assert_eq!(argv, ["open", "-b", "com.apple.Safari"]);
    }

    #[test]
    fn macos_passes_custom_args_after_args() {
        let argv = build_launch_argv_on(
            true,
            &bundle("com.apple.Safari"),
            None,
            Some("--private 'a b'"),
        )
        .unwrap();
        assert_eq!(
            argv,
            [
                "open",
                "-b",
                "com.apple.Safari",
                "--args",
                "--private",
                "a b"
            ]
        );
    }

    #[test]
    fn macos_needs_a_bundle_id() {
        assert_eq!(
            build_launch_argv_on(true, &bundle(" "), None, None),
            Err(ExecError::Empty)
        );
    }

    #[test]
    fn macos_exec_override_runs_as_written() {
        let argv = build_launch_argv_on(
            true,
            &bundle("x"),
            Some("/usr/local/bin/tool --flag"),
            Some("more"),
        )
        .unwrap();
        assert_eq!(argv, ["/usr/local/bin/tool", "--flag", "more"]);
    }
    #[cfg(not(target_os = "macos"))]
    use std::io::Write;

    fn entry(exec: &str) -> AppEntry {
        AppEntry {
            id: "org.example.App".to_string(),
            name: "Example App".to_string(),
            window_class: "example".to_string(),
            exec: exec.to_string(),
            icon: Some("example-icon".to_string()),
            path: PathBuf::from("/usr/share/applications/org.example.App.desktop"),
        }
    }

    fn desktop_argv(exec: &str) -> Result<Vec<String>, ExecError> {
        let e = entry(exec);
        parse_desktop_exec(&e.exec, &ExecContext::for_entry(&e))
    }

    #[test]
    fn drops_file_and_url_field_codes() {
        assert_eq!(
            desktop_argv("firefox %u --new-window %F").unwrap(),
            vec!["firefox", "--new-window"]
        );
    }

    #[test]
    fn leaves_plain_exec_untouched() {
        assert_eq!(desktop_argv("kate").unwrap(), vec!["kate"]);
    }

    // The next three `Exec=` lines are real ones from .desktop files on the
    // development machine. `DesktopEntry::parse_exec` from
    // freedesktop-desktop-entry 0.8.3 rejects the first two ("unmatched
    // quote") and splits the third's quoted argument into three pieces
    // (https://github.com/pop-os/freedesktop-desktop-entry/issues/36).
    #[test]
    fn real_quoted_program_path() {
        assert_eq!(
            desktop_argv("\"/usr/bin/opendeck\" %u").unwrap(),
            vec!["/usr/bin/opendeck"]
        );
        assert_eq!(
            desktop_argv("\"/home/user/.local/bin/claude\" --handle-uri %u").unwrap(),
            vec!["/home/user/.local/bin/claude", "--handle-uri"]
        );
    }

    #[test]
    fn real_quoted_argument_with_spaces() {
        assert_eq!(
            desktop_argv(
                "/usr/libexec/ibus-ui-gtk3 --enable-wayland-im --exec-daemon --daemon-args \"--xim --panel disable\""
            )
            .unwrap(),
            vec![
                "/usr/libexec/ibus-ui-gtk3",
                "--enable-wayland-im",
                "--exec-daemon",
                "--daemon-args",
                "--xim --panel disable"
            ]
        );
    }

    #[test]
    fn real_flatpak_exec_line() {
        assert_eq!(
            desktop_argv("/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=firefox --file-forwarding org.mozilla.firefox @@u %u @@").unwrap(),
            vec![
                "/usr/bin/flatpak", "run", "--branch=stable", "--arch=x86_64", "--command=firefox",
                "--file-forwarding", "org.mozilla.firefox", "@@u", "@@"
            ]
        );
    }

    #[test]
    fn shell_metacharacters_are_plain_arguments() {
        assert_eq!(
            desktop_argv("app $(touch /tmp/pwned) ; rm -rf ~ `id`").unwrap(),
            vec![
                "app",
                "$(touch",
                "/tmp/pwned)",
                ";",
                "rm",
                "-rf",
                "~",
                "`id`"
            ]
        );
    }

    #[test]
    fn quoted_escapes_and_string_escapes() {
        assert_eq!(
            desktop_argv(r#"sh-free "a \"quoted\" \$HOME \\ \`x\`" b\sc"#).unwrap(),
            // `\s` is a *string* escape for a space, which the quoting pass
            // then treats as a separator, as the spec requires.
            vec!["sh-free", r#"a "quoted" $HOME \ `x`"#, "b", "c"]
        );
    }

    #[test]
    fn percent_handling() {
        assert_eq!(desktop_argv("app 100%%").unwrap(), vec!["app", "100%"]);
        assert_eq!(desktop_argv("app --url=%u").unwrap(), vec!["app", "--url="]);
        assert_eq!(
            desktop_argv("app %i %c %k").unwrap(),
            vec![
                "app",
                "--icon",
                "example-icon",
                "Example App",
                "/usr/share/applications/org.example.App.desktop"
            ]
        );
        assert_eq!(desktop_argv("app %d %D %n %N %v %m").unwrap(), vec!["app"]);
        assert_eq!(desktop_argv("app \"%u\"").unwrap(), vec!["app", "%u"]);
    }

    #[test]
    fn rejects_empty_and_unterminated_exec() {
        assert_eq!(desktop_argv(""), Err(ExecError::Empty));
        assert_eq!(desktop_argv("%u %F"), Err(ExecError::Empty));
        assert_eq!(desktop_argv("\"app"), Err(ExecError::UnterminatedQuote));
    }

    #[test]
    fn exec_override_is_split_without_a_shell() {
        let argv = build_launch_argv_on(
            false,
            &entry("ignored"),
            Some("firefox --private-window 'two words' $(id)"),
            None,
        )
        .unwrap();
        assert_eq!(
            argv,
            vec!["firefox", "--private-window", "two words", "$(id)"]
        );
    }

    #[test]
    fn custom_args_are_appended_as_separate_arguments() {
        let argv = build_launch_argv_on(
            false,
            &entry("firefox %u"),
            None,
            Some("--new-window \"https://example.com/a b\"; reboot"),
        )
        .unwrap();
        assert_eq!(
            argv,
            vec![
                "firefox",
                "--new-window",
                "https://example.com/a b;",
                "reboot"
            ]
        );
    }

    #[test]
    fn exec_override_drops_pasted_field_codes() {
        let argv = build_launch_argv_on(false, &entry("x"), Some("firefox %u"), None).unwrap();
        assert_eq!(argv, vec!["firefox"]);
    }

    #[test]
    fn bad_user_quoting_is_an_error_not_a_guess() {
        assert!(matches!(
            build_launch_argv_on(false, &entry("x"), Some("firefox 'unterminated"), None),
            Err(ExecError::BadArguments(_))
        ));
        assert!(matches!(
            build_launch_argv_on(false, &entry("x"), Some("   "), None),
            Err(ExecError::Empty)
        ));
    }

    #[test]
    fn an_override_starting_with_an_option_is_rejected() {
        assert_eq!(
            build_launch_argv_on(false, &entry("x"), Some("--env=X=1 foo"), None),
            Err(ExecError::NotAProgram("--env=X=1".into()))
        );
        assert_eq!(
            build_launch_argv_on(true, &bundle("x"), Some("-foo"), None),
            Err(ExecError::NotAProgram("-foo".into()))
        );
    }

    #[test]
    fn an_override_starting_with_an_empty_word_is_rejected() {
        assert_eq!(
            build_launch_argv_on(false, &entry("x"), Some("'' foo"), None),
            Err(ExecError::NotAProgram(String::new()))
        );
    }

    #[test]
    fn an_exec_line_starting_with_an_option_is_rejected() {
        assert_eq!(
            build_launch_argv_on(false, &entry("-foo --bar"), None, None),
            Err(ExecError::NotAProgram("-foo".into()))
        );
        assert_eq!(
            build_launch_argv_on(false, &entry("\"\" foo"), None, None),
            Err(ExecError::NotAProgram(String::new()))
        );
    }

    #[test]
    fn custom_args_may_start_with_a_dash() {
        let argv =
            build_launch_argv_on(false, &entry("firefox"), None, Some("--new-window")).unwrap();
        assert_eq!(argv, vec!["firefox", "--new-window"]);
    }

    #[test]
    fn flatpak_sandbox_wraps_the_argv_without_a_shell() {
        let argv = vec!["firefox".to_string(), "--new-window".to_string()];
        assert_eq!(host_argv(&argv, false), argv);
        assert_eq!(
            host_argv(&argv, true),
            vec!["flatpak-spawn", "--host", "firefox", "--new-window"]
        );
    }

    #[tokio::test]
    async fn launching_a_missing_binary_fails_instead_of_reporting_success() {
        let result = SystemLauncher.launch(&["opendeck-focus-launcher-no-such-binary".to_string()]);
        let err = result.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn resolve_window_class_prefers_startup_wm_class() {
        assert_eq!(
            resolve_window_class("org.mozilla.firefox", Some("firefox")),
            "firefox"
        );
    }

    #[test]
    fn resolve_window_class_falls_back_to_entry_id() {
        assert_eq!(resolve_window_class("org.kde.kate", None), "org.kde.kate");
        assert_eq!(
            resolve_window_class("org.kde.kate", Some("")),
            "org.kde.kate"
        );
        assert_eq!(
            resolve_window_class("org.kde.kate", Some("  ")),
            "org.kde.kate"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn discovers_a_fixture_desktop_entry() {
        let dir = tempfile::tempdir().unwrap();
        let apps_dir = dir.path().join("applications");
        std::fs::create_dir_all(&apps_dir).unwrap();
        let mut file = std::fs::File::create(apps_dir.join("test-app.desktop")).unwrap();
        writeln!(
            file,
            "[Desktop Entry]\nType=Application\nName=Test App\nExec=test-app %u\nIcon=test-icon\nStartupWMClass=testapp\n"
        )
        .unwrap();

        let apps = list_installed_apps_from_paths(vec![dir.path().to_path_buf()], &[]);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Test App");
        assert_eq!(apps[0].window_class, "testapp");
        assert_eq!(apps[0].exec, "test-app %u");
        assert_eq!(apps[0].icon.as_deref(), Some("test-icon"));
        assert_eq!(apps[0].path, apps_dir.join("test-app.desktop"));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn hides_nodisplay_entries() {
        let dir = tempfile::tempdir().unwrap();
        let apps_dir = dir.path().join("applications");
        std::fs::create_dir_all(&apps_dir).unwrap();
        let mut file = std::fs::File::create(apps_dir.join("hidden-app.desktop")).unwrap();
        writeln!(
            file,
            "[Desktop Entry]\nType=Application\nName=Hidden App\nExec=hidden-app\nNoDisplay=true\n"
        )
        .unwrap();

        let apps = list_installed_apps_from_paths(vec![dir.path().to_path_buf()], &[]);
        assert!(apps.is_empty());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn hides_non_application_entries() {
        let dir = tempfile::tempdir().unwrap();
        let apps_dir = dir.path().join("applications");
        std::fs::create_dir_all(&apps_dir).unwrap();
        let mut file = std::fs::File::create(apps_dir.join("a-link.desktop")).unwrap();
        writeln!(
            file,
            "[Desktop Entry]\nType=Link\nName=Some Link\nURL=https://example.com\n"
        )
        .unwrap();

        let apps = list_installed_apps_from_paths(vec![dir.path().to_path_buf()], &[]);
        assert!(apps.is_empty());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn dedups_an_id_present_in_two_search_paths_keeping_the_first() {
        let user_dir = tempfile::tempdir().unwrap();
        let user_apps = user_dir.path().join("applications");
        std::fs::create_dir_all(&user_apps).unwrap();
        let mut user_file = std::fs::File::create(user_apps.join("dup-app.desktop")).unwrap();
        writeln!(
            user_file,
            "[Desktop Entry]\nType=Application\nName=User Override\nExec=dup-app\n"
        )
        .unwrap();

        let system_dir = tempfile::tempdir().unwrap();
        let system_apps = system_dir.path().join("applications");
        std::fs::create_dir_all(&system_apps).unwrap();
        let mut system_file = std::fs::File::create(system_apps.join("dup-app.desktop")).unwrap();
        writeln!(
            system_file,
            "[Desktop Entry]\nType=Application\nName=System Copy\nExec=dup-app\n"
        )
        .unwrap();

        // Search the user dir first, matching real XDG precedence order.
        let apps = list_installed_apps_from_paths(
            vec![
                user_dir.path().to_path_buf(),
                system_dir.path().to_path_buf(),
            ],
            &[],
        );

        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "User Override");
    }

    /// Writes `body` as `<id>.desktop` under `<root>/applications`.
    #[cfg(not(target_os = "macos"))]
    fn write_entry(root: &Path, id: &str, body: &str) {
        let apps_dir = root.join("applications");
        std::fs::create_dir_all(&apps_dir).unwrap();
        let mut file = std::fs::File::create(apps_dir.join(format!("{id}.desktop"))).unwrap();
        writeln!(file, "[Desktop Entry]\n{body}").unwrap();
    }

    /// A user copy of `dup-app` with `user_extra`, then a visible system copy.
    #[cfg(not(target_os = "macos"))]
    fn user_and_system_copies(user_extra: &str, desktops: &[String]) -> Vec<AppEntry> {
        let user_dir = tempfile::tempdir().unwrap();
        write_entry(
            user_dir.path(),
            "dup-app",
            &format!("Type=Application\nName=User Copy\nExec=dup-app\n{user_extra}"),
        );
        let system_dir = tempfile::tempdir().unwrap();
        write_entry(
            system_dir.path(),
            "dup-app",
            "Type=Application\nName=System Copy\nExec=dup-app\n",
        );
        list_installed_apps_from_paths(
            vec![
                user_dir.path().to_path_buf(),
                system_dir.path().to_path_buf(),
            ],
            desktops,
        )
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_hidden_user_copy_hides_the_system_copy() {
        assert_eq!(user_and_system_copies("Hidden=true\n", &[]), vec![]);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_nodisplay_user_copy_hides_the_system_copy() {
        assert_eq!(user_and_system_copies("NoDisplay=true\n", &[]), vec![]);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_user_copy_meant_for_another_desktop_hides_the_system_copy() {
        let kde = vec!["KDE".to_string()];
        assert_eq!(user_and_system_copies("NotShowIn=KDE;\n", &kde), vec![]);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn leaves_out_entries_not_shown_on_the_current_desktop() {
        let dir = tempfile::tempdir().unwrap();
        write_entry(
            dir.path(),
            "kdesystemsettings",
            "Type=Application\nName=Settings Elsewhere\nExec=x\nNotShowIn=KDE;\n",
        );
        write_entry(
            dir.path(),
            "gnome-only",
            "Type=Application\nName=GNOME Only\nExec=y\nOnlyShowIn=GNOME;\n",
        );
        write_entry(
            dir.path(),
            "everywhere",
            "Type=Application\nName=Everywhere\nExec=z\n",
        );
        let names = |desktops: &[String]| {
            list_installed_apps_from_paths(vec![dir.path().to_path_buf()], desktops)
                .into_iter()
                .map(|a| a.name)
                .collect::<Vec<_>>()
        };

        assert_eq!(names(&["KDE".to_string()]), vec!["Everywhere"]);
        assert_eq!(
            names(&["GNOME".to_string()]),
            vec!["Everywhere", "GNOME Only", "Settings Elsewhere"]
        );
        assert_eq!(names(&[]).len(), 3);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn shown_on_follows_only_and_not_show_in() {
        let kde = vec!["KDE".to_string()];
        let gnome = vec!["GNOME".to_string()];
        // The crate splits on ';', so a trailing ';' leaves an empty item.
        assert!(!shown_on(None, Some(vec!["KDE", ""]), &kde));
        assert!(shown_on(None, Some(vec!["KDE", ""]), &gnome));
        assert!(!shown_on(Some(vec!["GNOME", ""]), None, &kde));
        assert!(shown_on(Some(vec!["GNOME", ""]), None, &gnome));
        assert!(shown_on(Some(vec!["GNOME"]), Some(vec!["KDE"]), &[]));
        assert!(shown_on(None, None, &kde));
        assert!(!shown_on(None, Some(vec!["kde"]), &kde));
        assert!(shown_on(Some(vec!["kde"]), None, &kde));
        // XDG_CURRENT_DESKTOP can name several desktops; any match counts.
        let ubuntu = vec!["ubuntu".to_string(), "GNOME".to_string()];
        assert!(shown_on(Some(vec!["GNOME"]), None, &ubuntu));
        assert!(!shown_on(None, Some(vec!["GNOME"]), &ubuntu));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn current_desktops_splits_on_colons_and_drops_blanks() {
        assert_eq!(
            current_desktops(Some("ubuntu:GNOME")),
            vec!["ubuntu", "GNOME"]
        );
        assert_eq!(current_desktops(Some("KDE")), vec!["KDE"]);
        assert!(current_desktops(Some("")).is_empty());
        assert!(current_desktops(None).is_empty());
        assert_eq!(current_desktops(Some(":KDE:")), vec!["KDE"]);
    }
}
