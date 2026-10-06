# OpenDeck Focus Launcher

An [OpenDeck](https://github.com/nekename/OpenDeck) plugin with one action,
**Focus or Launch**: press a key bound to an app, and it brings that app's
window to the front if it has one, or launches it if it doesn't. Press again
while the app is already focused to cycle to its next window, or minimize it
if it only has one.

Built because OpenDeck's own Starter Pack app-launcher action always starts a
new process, even when the app is already open.

## Supported desktops

- **KDE Plasma** (X11 or Wayland) — via [`kdotool`](https://github.com/jinliu/kdotool),
  which must be installed and on `PATH`.
- **GNOME Shell** (X11 or Wayland) — *experimental, never run against a live
  session* — via the [Window Calls](https://github.com/ickyicky/window-calls)
  GNOME Shell extension, which must be installed and enabled.
- **Other window managers in an X11 session** — *experimental, never run
  against a live session* — via `wmctrl` and `xdotool`, which must be
  installed and on `PATH`.

- **macOS** (Apple Silicon) — via the Accessibility API. Apps are the `.app`
  bundles in `/Applications`, `/System/Applications` (and their `Utilities`)
  and `~/Applications`, matched by **bundle id** (e.g. `com.apple.Safari`).
  Focusing, cycling, minimising and closing windows need the Accessibility
  permission for OpenDeck: macOS asks the first time, or allow it in **System
  Settings › Privacy & Security › Accessibility**. Launching an app that isn't
  running works without it. Icons come from the app bundle (`.icns`, converted
  with the built-in `sips` and cached in `~/Library/Caches/opendeck-focus-launcher/`).

Other Wayland compositors (sway, Hyprland, niri, ...) are not supported: their
native windows are invisible to `wmctrl`, so the plugin does not guess. If no
backend fits, it logs why and every key press shows an alert instead of
acting.

A key matches windows whose class (or WM_CLASS instance name) is exactly the
app's window class, ignoring case: `firefox` never matches `firefox-esr`.

## Installing

Download `opendeck-focus-launcher.streamDeckPlugin` from the latest
[release](https://github.com/jfms7s/opendeck-focus-launcher/releases) (one
bundle with Linux x86_64 and aarch64 and macOS Apple Silicon binaries; check it
against the release's `SHA256SUMS` — on macOS with `shasum -a 256 -c SHA256SUMS`),
then either double-click it (if your file manager associates the extension with
OpenDeck) or unzip it into OpenDeck's plugins folder and restart OpenDeck
(plugins are only loaded at startup):

- Linux: `~/.config/opendeck/plugins/`
- macOS: `~/Library/Application Support/opendeck/plugins/`

On macOS, a bundle unzipped by hand (e.g. in Finder) is marked as downloaded
and Gatekeeper refuses to start the binary. Clear the mark once:

```bash
xattr -dr com.apple.quarantine ~/Library/Application\ Support/opendeck/plugins/com.jfms7s.focuslauncher.sdPlugin
```

## Using a key

1. Add a **Focus or Launch** key in OpenDeck.
2. **App** — pick an app from the dropdown. Its `.desktop` file path is shown
   underneath for reference. If the saved app is no longer installed it stays
   selected, marked "(not installed)", and the key alerts when pressed.
3. **Behaviour** — leave "Cycle to the next window if several are open" and
   "Minimize when already focused" checked for the default behavior described
   above. Unchecking the first means a repeat press does nothing (instead of
   cycling) once the app has several windows open and one is focused;
   unchecking the second means a repeat press does nothing (instead of
   minimizing) when the app's single window is already focused. Check "Press
   and hold to close all windows" to make holding the key or dial (past
   ~500 ms) close every window of that app instead. Off by default.
4. **Appearance & matching** and **Launch** — optional overrides for the
   selected app. Each field shows the app's own value as its placeholder and
   only takes effect once you type something in it:
   - **Name** — the key's title.
   - **Icon name** — an icon theme name, or an absolute path to an image, to
     show instead of the app's own icon.
   - **Window class** — only needed if the app's windows report a different
     class than its `.desktop` file declares (letters, digits, spaces and
     `. _ - +` only).
   - **Command** — replaces the app's launch command.
   - **Extra arguments** — appended to the command.

   Commands are run directly, never through a shell: quotes group words, but
   `$(...)`, backticks, `;`, `|` and globs are plain text. Picking a different
   app resets every override on that key.

Some apps' running windows don't use the class their `.desktop` file
declares. Chrome/Chromium PWAs are handled automatically (the key retries
with the desktop id). For others, such as the Plex snap, find the real class
(`kdotool search --class . getwindowclassname %@` on KDE; on macOS the bundle
id, `osascript -e 'id of app "Safari"'`) and set it as the
Window class override.

## Manual smoke-test checklist

The decision logic and the tool-output parsers are unit-tested, but the real
calls into kdotool, Window Calls and wmctrl/xdotool, the OpenDeck events and
the property inspector in OpenDeck are only checked by hand. **None of these
rows has a recorded run yet.** Run them against a live session before
publishing a release draft, and record the result (date, version, OK / FAIL
and notes) in the release notes.

| # | Check | KDE Plasma | GNOME Shell | X11 WM | macOS |
|---|---|---|---|---|---|
| 1 | App with no window open → press launches it once. | not yet run | not yet run | not yet run | not yet run |
| 2 | Press twice quickly while a slow app (browser) cold-starts → only one instance. | not yet run | not yet run | not yet run | not yet run |
| 3 | App running in the background → press focuses its window. | not yet run | not yet run | not yet run | not yet run |
| 4 | App focused, one window → press minimizes it. | not yet run | not yet run | not yet run | not yet run |
| 5 | App focused, three windows, cycling on → three presses visit all three, wrapping. | not yet run | not yet run | not yet run | not yet run |
| 6 | Cycling off, app focused, several windows → press does nothing. | not yet run | not yet run | not yet run | not yet run |
| 7 | Minimize-when-focused off, app focused, one window → press does nothing. | not yet run | not yet run | not yet run | not yet run |
| 8 | Close-all-on-hold on, app has several windows, plus a similarly named app open (e.g. Firefox and Firefox ESR) → holding closes only the app's own windows; a tap still focuses/cycles/minimizes. | not yet run | not yet run | not yet run | not yet run |
| 9 | Close-all-on-hold off → holding does the same as a tap. | not yet run | not yet run | not yet run | not yet run |
| 10 | Close-all-on-hold on a dial press → same as on a key. | not yet run | not yet run | not yet run | not yet run |
| 11 | App with a reverse-DNS class (e.g. `org.kde.kate`) → focuses instead of relaunching. | not yet run | not yet run | not yet run | not yet run |
| 12 | Selecting an app sets the key's title and icon. | not yet run | not yet run | not yet run | not yet run |
| 13 | Name/Icon/Window class/Command/Extra arguments overrides change the title, icon, matching and launch; clearing them reverts. | not yet run | not yet run | not yet run | not yet run |
| 14 | Picking a different app resets those overrides; editing a checkbox never clears the selected app. | not yet run | not yet run | not yet run | not yet run |
| 15 | Kill the backend tool (e.g. rename `kdotool`) → press shows an alert and launches nothing. | not yet run | not yet run | not yet run | not yet run |
| 16 | macOS without the Accessibility permission: a quit app launches; a running app alerts and the log names the System Settings path. | n/a | n/a | n/a | not yet run |
| 17 | macOS: the app list shows bundles from /Applications and /System/Applications, and their icons render on the keys. | n/a | n/a | n/a | not yet run |
| 18 | macOS: an app hidden with ⌘H → press shows it and focuses its window. | n/a | n/a | n/a | not yet run |
| 19 | macOS: an app with 3 windows → repeated presses visit each window in turn (cycling doesn't get stuck on two). | n/a | n/a | n/a | not yet run |

The plugin log (`~/.local/share/opendeck/logs/plugins/com.jfms7s.focuslauncher.sdPlugin.log`;
on macOS `~/Library/Logs/opendeck/plugins/com.jfms7s.focuslauncher.sdPlugin.log`)
records each press: the class searched, the matching window ids, the focused
window and what was done, which is the evidence to note for each row.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked                          # unit tests (no live desktop needed)
node --test tests/*.test.mjs                 # property inspector tests
cargo build --release --locked               # or --target <triple>, once per CodePaths target
node build.mjs                               # assembles dist/<uuid>.sdPlugin from every built target
cp -r dist/com.jfms7s.focuslauncher.sdPlugin ~/.config/opendeck/plugins/
# restart OpenDeck, then work through the smoke-test checklist above
```

`cargo test --locked -- --ignored --nocapture` also runs a test that resolves
every installed app's icon against your real icon theme and prints timings.

The tests in `src/backend/` parse output captured from the real tools
(`tests/fixtures/`). The Window Calls fixture follows the extension's own
`List()` source, since no GNOME session was available to capture one.

## Releasing

1. Bump `version` in `Cargo.toml` and `Version` in `assets/manifest.json`
   together (`node build.mjs --check-only` verifies they match).
2. Push a `vX.Y.Z` tag. The release workflow checks the tag matches both
   versions, runs fmt, clippy and the tests, builds both Linux architectures
   and the macOS one (on a macOS runner), and
   creates a **draft** release with the bundle and `SHA256SUMS` attached.
3. Run the smoke-test checklist against that bundle, note the results in the
   draft's notes, then publish it. Publishing makes it the latest release,
   which is what the Ansible role installs.

## License

MIT — see [LICENSE](LICENSE).
