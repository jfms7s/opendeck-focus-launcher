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
- **GNOME Shell** — via the [Window Calls](https://github.com/ickyicky/window-calls)
  GNOME Shell extension, which must be installed and enabled.
- **Other X11 window managers** — via `wmctrl` and `xdotool`, which must be
  installed and on `PATH`.

If none of the above is detected, the plugin logs why and takes no action
rather than guessing.

## Installing

Download the latest `.streamDeckPlugin` for your architecture from
[Releases](https://github.com/jfms7s/opendeck-focus-launcher/releases), then
either double-click it (if your file manager associates the extension with
OpenDeck) or unzip it into `~/.config/opendeck/plugins/` and restart OpenDeck
(plugins are only loaded at startup).

## Using a key

1. Add a **Focus or Launch** key in OpenDeck.
2. Pick an app from the dropdown. Its `.desktop` file path is shown
   underneath for reference.
3. Leave "Cycle to next window" and "Minimize when already focused" checked
   for the default behavior described above. Unchecking "Cycle to next
   window" means a repeat press does nothing (instead of cycling) once the
   app has several windows open and one is already focused; unchecking
   "Minimize when already focused" means a repeat press does nothing
   (instead of minimizing) when the app has a single window and it's already
   focused.
4. Expand **Advanced overrides** to fine-tune the selected app — each field
   is auto-filled (shown as its placeholder hint where applicable) and only
   takes effect once you actually type something in it:
   - **Window class** — only needed if the app reports the wrong one.
   - **Name** — sets the key's title; leave blank to use the app's own name.
   - **Icon name** — an icon theme name to resolve instead of the app's own
     icon.
   - **Exec** — overrides the launch command.
   - **Custom arguments** — extra arguments appended when launching.

   Picking a different app resets every override on that key back to unset.

## Manual smoke-test checklist

Run this against a live desktop session before cutting a release (only KDE
Plasma has been verified so far — see the entries below):

- [ ] App with no window open → key press launches it. *(KDE: verified / not yet verified)*
- [ ] App running in the background → key press focuses its window. *(KDE: verified / not yet verified)*
- [ ] App focused, one window → key press minimizes it. *(KDE: verified / not yet verified)*
- [ ] App focused, several windows, cycling on → key press moves to the next
      one, wrapping back to the first. *(KDE: verified / not yet verified)*
- [ ] Cycling off, app focused, several windows → key press does nothing. *(KDE: verified / not yet verified)*
- [ ] Minimize-when-focused off, app focused, one window → key press does
      nothing. *(KDE: verified / not yet verified)*
- [ ] GNOME Shell, same checks above, with the Window Calls extension
      installed. *(not yet verified — no GNOME session available during
      development)*
- [ ] Plain X11 window manager, same checks above. *(not yet verified — no
      X11 session available during development)*
- [ ] Selecting an app sets the key's title and icon to match it. *(KDE: verified / not yet verified)*
- [ ] Setting Name/Icon/Exec/Custom arguments overrides changes the key's
      title, icon, and launch behavior accordingly; clearing them reverts to
      the app's own values. *(KDE: verified / not yet verified)*
- [ ] Picking a different app resets those overrides. *(KDE: verified / not yet verified)*

## Development

```bash
cargo test                                   # unit tests (no live desktop needed)
cargo build --release --target <triple>
node build.mjs <triple>                      # assembles dist/<uuid>.sdPlugin
cp -r dist/com.jfms7s.focuslauncher.sdPlugin ~/.config/opendeck/plugins/
# restart OpenDeck, then work through the smoke-test checklist above
```

## License

MIT — see [LICENSE](LICENSE).
