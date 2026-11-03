# OpenDeck Focus Launcher

An [OpenDeck](https://github.com/nekename/OpenDeck) plugin with one action,
**Focus or Launch**: press a key bound to an app, and it brings that app's
window to the front if it has one, or launches it if it doesn't. Press again
while the app is already focused to cycle to its next window, or minimize it
if it only has one.

Built because the Linux App Launcher plugin's launch action
(`me.amankhanna.oadesktopentry`), the usual way to start apps from OpenDeck on
Linux, always starts a new process, even when the app is already open.

## Supported desktops

- **KDE Plasma** (X11 or Wayland) — via [`kdotool`](https://github.com/jinliu/kdotool),
  which must be installed and on `PATH`.
- **GNOME Shell** (X11 or Wayland) — *experimental, never run against a live
  session* — via the [Window Calls](https://github.com/ickyicky/window-calls)
  GNOME Shell extension, which must be installed and enabled.
- **Other window managers in an X11 session** — *experimental, never run
  against a live session* — via `wmctrl` and `xdotool`, which must be
  installed and on `PATH`.

- **macOS** (Apple Silicon) — *preview, never run on a Mac* — via the
  Accessibility API. Apps are the `.app` bundles in `/Applications`,
  `/System/Applications` (and their `Utilities`) and `~/Applications`,
  matched by **bundle id** (e.g. `com.apple.Safari`). Focusing, cycling,
  minimising and closing windows need the Accessibility permission for
  OpenDeck: macOS asks the first time, or allow it in **System Settings ›
  Privacy & Security › Accessibility**. Launching an app that isn't
  running works without it. After updating OpenDeck, if running apps' keys
  start alerting, remove OpenDeck from Accessibility and add it again (macOS
  ties the permission to OpenDeck's signature). Icons come from the app
  bundle (`.icns`, converted with the built-in `sips` and cached in
  `~/Library/Caches/opendeck-focus-launcher/`).

- **OpenDeck installed as a Flatpak** (Linux) — *experimental, never run
  against a live session*. Every host program the plugin runs (the app it
  launches, `kdotool`, `wmctrl`/`xdotool`, `gsettings`) goes through
  `flatpak-spawn --host`, so those tools must be installed on the host, not in
  the sandbox. Apps are read from the host's application directories only:
  `~/.local/share/applications`, the Flatpak exports
  (`~/.local/share/flatpak/exports/share/applications`,
  `/var/lib/flatpak/exports/share/applications`) and the host's
  `/usr/share/applications` as `/run/host/usr/share/applications`. Which of
  them the plugin sees depends on OpenDeck's Flatpak permissions; all of
  them are visible with
  `flatpak override --user --talk-name=org.freedesktop.Flatpak --filesystem=host-os:ro --filesystem=xdg-data/applications:ro --filesystem=xdg-data/flatpak/exports/share/applications:ro --filesystem=/var/lib/flatpak/exports/share/applications:ro <OpenDeck's app id>`
  (`org.freedesktop.Flatpak` is what lets `flatpak-spawn --host` run
  anything). The GNOME backend talks to GNOME Shell over the session bus, so
  OpenDeck also needs `--talk-name=org.gnome.Shell`. Icons
  are looked up in the sandbox's icon themes only, so host-only icons fall
  back to the default key image.

Other Wayland compositors (sway, Hyprland, niri, ...) are not supported: their
native windows are invisible to `wmctrl`, so the plugin does not guess. If no
backend fits, it logs why and every key press shows an alert instead of
acting. On Linux, a backend call (kdotool, Window Calls, wmctrl or xdotool)
that hasn't answered within 3 seconds is cancelled and the key alerts, so one
stuck tool can't freeze the other keys.

A key matches windows whose class (or WM_CLASS instance name) is exactly the
app's window class, ignoring case: `firefox` never matches `firefox-esr`.

## Installing

Download `opendeck-focus-launcher.streamDeckPlugin` from the latest
[release](https://github.com/jfms7s/opendeck-focus-launcher/releases) (one
bundle with Linux x86_64 and macOS Apple Silicon binaries; check it
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
2. **App** — pick an app from the dropdown. Its `.desktop` file path (the
   `.app` bundle on macOS) is shown underneath for reference. If the saved app is no longer installed it stays
   selected, marked "(not installed)", and the key alerts when pressed.
   Below it, "Windows open now: N" counts the windows a press would act on
   (after the Window class and title overrides), updated as you change
   settings; "unknown" comes with the reason (e.g. no window backend).
   On Linux the list follows your menu: an app you hid (a `Hidden=true` or
   `NoDisplay=true` copy in `~/.local/share/applications`) and entries meant
   for another desktop (`OnlyShowIn=`/`NotShowIn=`) are left out.
3. **Behaviour** — leave "Cycle to the next window if several are open" and
   "Minimize when already focused" checked for the default behavior described
   above. Unchecking the first means a repeat press does nothing (instead of
   cycling) once the app has several windows open and one is focused;
   unchecking the second means a repeat press does nothing (instead of
   minimizing) when the app's single window is already focused. **Hold**
   sets what holding the key or dial (past ~500 ms) does: *Same as tap*
   (the default), *Open another window* (runs the app's launch command, with
   its Extra arguments, even when it already has windows; a second hold
   within 4 s does nothing while the first is starting), or *Close all
   windows* (closes every window of that app). *Open another window* can't
   do more than the app's own command: a single-instance app such as Steam
   just comes forward, and on macOS (`open -b`) a running app does the same
   unless a Command override opens a new window. Keys saved by an
   older version with "Press and hold to close all windows" checked open as
   *Close all windows*. **Only windows on the current desktop** (off by
   default) makes a window on another virtual desktop count as not open, so
   the key focuses or cycles only among the app's windows on this desktop
   (plus any shown on every desktop) and launches a new one here when there
   are none, instead of switching desktops. KDE Plasma only (a window on
   several, but not all, desktops counts on its first one); on GNOME, X11
   and macOS the option is ignored and the log says so once.
4. **Appearance & matching** and **Launch** — optional overrides for the
   selected app. Each field shows the app's own value as its placeholder and
   only takes effect once you type something in it:
   - **Name** — the key's title.
   - **Icon name** — an icon theme name, or an absolute path to an image, to
     show instead of the app's own icon (on macOS only an absolute path works,
     to an image or an `.icns`).
   - **Window class** — only needed if the app's windows report a different
     class than the app declares (its `.desktop` file's `StartupWMClass` or
     id; the bundle id on macOS) (letters, digits, spaces and
     `. _ - +` only). On the X11 backend (wmctrl) a class can't contain
     spaces, so a Steam game whose class is its name needs a `steam_app_<n>`
     override.
   - **Window title contains** — only the app's windows whose title contains
     this text (ignoring case) belong to the key: a press focuses and cycles
     among those, holding (with Hold set to *Close all windows*) closes only those, and with none
     open the key launches. Combine it with **Extra arguments** to open
     exactly that window, e.g. title `Grafana` with arguments
     `--new-window https://grafana.example`. Titles change with the open tab
     or file, so match the project or site, not the page. KDE, GNOME and X11
     only: on macOS the filter is ignored (the log says so) and the key
     matches by app as usual.
   - **Command** — replaces the app's launch command.
   - **Extra arguments** — appended to the command.

   Commands are run directly, never through a shell: quotes group words, but
   `$(...)`, backticks, `;`, `|` and globs are plain text. Picking a different
   app resets every override on that key.

Some apps' running windows don't use the class their `.desktop` file
declares (on macOS the class is always the app's bundle id). Chrome/Chromium
PWAs are handled automatically (the key retries with the desktop id). The
"Windows open now" line under the app shows whether the key finds the app's
open windows. If it says 0 while the app is open (e.g. the Plex snap), open
the app, reopen the key's settings and choose its window in **Pick from open
windows** under Window class (KDE, GNOME and X11; a class the plugin can't
match is listed but greyed out). On macOS, type the bundle id instead
(`osascript -e 'id of app "Safari"'`).

## Using a dial

The same action works on a dial (Stream Deck +, or a deck with knobs). A
press does what a key press does. Turning the dial steps through the app's
open windows without switching to them: the dial's title shows the window's
place (`2/3`) and, on KDE, GNOME and X11, its title. Pressing then brings up
the window shown; holding does the Hold action instead. After the press the
dial shows the app's name again. On macOS only the place is shown (the
macOS backend doesn't read window titles).

## Manual smoke-test checklist

The decision logic and the tool-output parsers are unit-tested, but the real
calls into kdotool, Window Calls and wmctrl/xdotool, the OpenDeck events and
the property inspector in OpenDeck are only checked by hand. **None of these
rows has a recorded run yet.** Run them against a live session before
publishing a release draft, and record the result (date, version, OK / FAIL
and notes) in the release notes.

Each draft release starts with this table as an unticked checklist
(`scripts/release-body.mjs`, from the markers around it). A release is
published only once its notes record a result for every row: passed, failed,
or not run and why. The maintainer keeps the full per-release smoke log
outside this repository.

<!-- smoke-checklist:start (scripts/release-body.mjs turns this table into each draft release's notes) -->
| # | Check | KDE Plasma | GNOME Shell | X11 WM | macOS |
|---|---|---|---|---|---|
| 1 | App with no window open → press launches it once. | not yet run | not yet run | not yet run | not yet run |
| 2 | Press twice quickly while a slow app (browser) cold-starts → only one instance. | not yet run | not yet run | not yet run | not yet run |
| 3 | App running in the background → press focuses its window. | not yet run | not yet run | not yet run | not yet run |
| 4 | App focused, one window → press minimizes it. | not yet run | not yet run | not yet run | not yet run |
| 5 | App focused, three windows, cycling on → three presses visit all three, wrapping. | not yet run | not yet run | not yet run | not yet run |
| 6 | Cycling off, app focused, several windows → press does nothing. | not yet run | not yet run | not yet run | not yet run |
| 7 | Minimize-when-focused off, app focused, one window → press does nothing. | not yet run | not yet run | not yet run | not yet run |
| 8 | Hold set to *Close all windows*, app has several windows, plus a similarly named app open (e.g. Firefox and Firefox ESR) → holding closes only the app's own windows; a tap still focuses/cycles/minimizes. | not yet run | not yet run | not yet run | not yet run |
| 9 | Hold set to *Same as tap* → holding does the same as a tap. | not yet run | not yet run | not yet run | not yet run |
| 10 | Hold set to *Close all windows*, on a dial press → same as on a key. | not yet run | not yet run | not yet run | not yet run |
| 11 | App with a reverse-DNS class (e.g. `org.kde.kate`) → focuses instead of relaunching. | not yet run | not yet run | not yet run | not yet run |
| 12 | Selecting an app sets the key's title and icon. | not yet run | not yet run | not yet run | not yet run |
| 13 | Name/Icon/Window class/Command/Extra arguments overrides change the title, icon, matching and launch; clearing them reverts. | not yet run | not yet run | not yet run | not yet run |
| 14 | Picking a different app resets those overrides; editing a checkbox never clears the selected app. | not yet run | not yet run | not yet run | not yet run |
| 15 | Kill the backend tool (e.g. rename `kdotool`) → press shows an alert and launches nothing. | not yet run | not yet run | not yet run | not yet run |
| 16 | macOS without the Accessibility permission: a quit app launches; a running app alerts and the log names the System Settings path. | n/a | n/a | n/a | not yet run |
| 17 | macOS: the app list shows bundles from /Applications and /System/Applications, and their icons render on the keys. | n/a | n/a | n/a | not yet run |
| 18 | macOS: an app hidden with ⌘H → press shows it and focuses its window. | n/a | n/a | n/a | not yet run |
| 19 | macOS: an app with 3 windows → repeated presses visit each window in turn (cycling doesn't get stuck on two). | n/a | n/a | n/a | not yet run |
| 20 | Make the backend hang (a `kdotool`, or `wmctrl` on X11, first on the PATH that runs `sleep 30`) → press alerts after about 3 s and launches nothing; another key works right after. | not yet run | n/a | not yet run | n/a |
| 21 | Two windows of one app with different titles, a key with "Window title contains" set to text in one title → press focuses only that window; change the filter to text in neither title → press launches (with its Extra arguments). | not yet run | not yet run | not yet run | n/a |
| 22 | An app removed after the key was set up (macOS: delete it from /Applications and press within a minute; Linux: OpenDeck installed as a Flatpak, the app's host binary removed) → press alerts, and a second press alerts again instead of doing nothing. | not yet run | not yet run | not yet run | not yet run |
| 23 | macOS: after an OpenDeck update, row 16's permission still works (else remove and re-add OpenDeck in Accessibility). | n/a | n/a | n/a | not yet run |
| 24 | Hold set to *Open another window*, app with Extra arguments `--new-window` (e.g. Firefox) and a window open → holding opens one new window; holding twice quickly opens only one. | not yet run | not yet run | not yet run | not yet run |
| 25 | Hold set to *Open another window* on a single-instance app (e.g. Steam) → holding brings it forward and opens nothing new; a key saved by v0.6 with close-all on shows *Close all windows*. | not yet run | not yet run | not yet run | not yet run |
| 26 | Open a key's settings with its app running in 2 windows → "Windows open now: 2"; set a Window class that matches nothing → it drops to 0; with the app closed → 0. | not yet run | not yet run | not yet run | not yet run |
| 27 | Key for an app whose windows report another class (e.g. the Plex snap), app open → reopen its settings, pick the window under "Pick from open windows" → Window class is filled, the count goes above 0, and a press focuses it. | not yet run | not yet run | not yet run | n/a |
| 28 | Dial bound to an app with three windows → turning shows `1/3`, `2/3`, `3/3` with each window's title and switches to none of them; turning back wraps; a press focuses the window shown and the dial shows the app's name again. | not yet run | not yet run | not yet run | not yet run |
| 29 | Dial: turn to a window, close that window, press → a normal tap (focuses the app's first window); with the app closed, turning shows "no windows" and a press launches it. | not yet run | not yet run | not yet run | not yet run |
| 30 | OpenDeck installed as a Flatpak, with the permissions under "Supported desktops" → the app list shows host apps; a press focuses an open host window and launches a closed one; without `kdotool` on the host (KDE) a press alerts that it is not installed. | not yet run | not yet run | not yet run | n/a |
| 31 | Two virtual desktops, app open only on desktop 2, key on desktop 1 with "Only windows on the current desktop" on → press launches a new window here, no desktop switch; with it off → press switches to desktop 2 and focuses the window. | not yet run | n/a | n/a | n/a |
<!-- smoke-checklist:end -->

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

A pull request's description needs a filled-in `## Verification` section (what
you ran and what you saw, as in `.github/pull_request_template.md`); the
`pr-verification` check fails without one. `cargo audit` runs on every pull
request, every Monday (`audit.yml`) and before every release draft.

## Releasing

1. Bump `version` in `Cargo.toml` and `Version` in `assets/manifest.json`
   together (`node build.mjs --check-only` verifies they match).
2. Push a `vX.Y.Z` tag. The release workflow checks the tag matches both
   versions, runs fmt, clippy, the tests and `cargo audit`, builds the Linux
   x86_64 binary and the macOS one (on a macOS runner, after running the tests
   there too), and
   creates a **draft** release with the bundle and `SHA256SUMS` attached. The
   draft's notes are the smoke-test checklist, unticked, plus GitHub's
   generated change list.
3. Run the smoke-test checklist against that bundle and record it in the
   draft's notes: tick what passed, mark the rest "fail" or "not run" with
   why, and fill in the smoke log line. Then publish it. Publishing makes it
   the latest release, which is what the Ansible role installs.

## License

MIT — see [LICENSE](LICENSE).
