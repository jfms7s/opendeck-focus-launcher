//! GNOME Shell backend: calls the Window Calls extension
//! (`org.gnome.Shell.Extensions.Windows`) on the session bus.
//!
//! Uses zbus's async API on the plugin's own tokio runtime, with one session
//! connection opened lazily and reused for every call. The connect and every
//! method call are bounded by `process::CALL_TIMEOUT`.

use super::process::{CALL_TIMEOUT, with_deadline};
use super::{BackendError, WindowBackend, WindowClass, WindowId, WindowSnapshot};
use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::OnceCell;

const DESTINATION: &str = "org.gnome.Shell";
const PATH: &str = "/org/gnome/Shell/Extensions/Windows";
const INTERFACE: &str = "org.gnome.Shell.Extensions.Windows";

pub struct GnomeWindowCallsBackend {
    connection: OnceCell<zbus::Connection>,
}

impl GnomeWindowCallsBackend {
    pub fn new() -> Self {
        Self {
            connection: OnceCell::new(),
        }
    }

    async fn connection(&self) -> Result<&zbus::Connection, BackendError> {
        self.connection
            .get_or_try_init(|| async {
                let connect = async {
                    zbus::Connection::session()
                        .await
                        .map_err(|e| BackendError::Unavailable(format!("no session D-Bus: {e}")))
                };
                // A bus that never answers the handshake is as good as none.
                with_deadline("session D-Bus connect", CALL_TIMEOUT, connect)
                    .await
                    .map_err(|e| match e {
                        BackendError::CommandFailed(msg) => BackendError::Unavailable(msg),
                        other => other,
                    })
            })
            .await
    }

    /// Calls a Window Calls method. `body` goes to zbus as-is: `()` is the
    /// empty signature and a bare `u32` is `"u"`. A 1-tuple `(id,)` would be
    /// the STRUCT signature `"(u)"`, which is wrong for these methods.
    async fn call<B>(&self, method: &'static str, body: &B) -> Result<zbus::Message, BackendError>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        let connection = self.connection().await?;
        // The session connection is shared and built without a method
        // timeout, so bound each call here: a Shell extension that never
        // replies would otherwise stall every key.
        with_deadline(&format!("Window Calls {method}"), CALL_TIMEOUT, async {
            connection
                .call_method(Some(DESTINATION), PATH, Some(INTERFACE), method, body)
                .await
                .map_err(|e| classify_call_error(&e))
        })
        .await
    }

    async fn list(&self) -> Result<Vec<GnomeWindow>, BackendError> {
        let reply = self.call("List", &()).await?;
        let json: String = reply
            .body()
            .deserialize()
            .map_err(|e| BackendError::CommandFailed(format!("bad Window Calls reply: {e}")))?;
        parse_all(&json)
    }
}

impl Default for GnomeWindowCallsBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// One entry of Window Calls' `List` reply. Field names follow the
/// extension's `List()` (`wm_class`, `wm_class_instance`, `title`, `id`, `focus`,
/// among others this plugin ignores). `wm_class`/`wm_class_instance` come
/// from Mutter getters that can return `null`. `focus` is required: if it is
/// missing the schema has changed, and silently assuming "not focused" would
/// stop minimize and cycling from ever firing.
#[derive(Debug, Deserialize)]
struct GnomeWindow {
    id: u64,
    wm_class: Option<String>,
    #[serde(default)]
    wm_class_instance: Option<String>,
    /// Read only for keys with a title filter; missing or `null` is `""`.
    #[serde(default)]
    title: Option<String>,
    focus: bool,
}

impl GnomeWindow {
    fn window_id(&self) -> WindowId {
        WindowId::new(self.id.to_string())
    }

    fn matches(&self, class: &WindowClass) -> bool {
        [&self.wm_class, &self.wm_class_instance]
            .into_iter()
            .flatten()
            .any(|candidate| class.matches(candidate))
    }
}

/// Parses the `List` reply and sorts it by id. Window Calls returns
/// `global.get_window_actors()`, which is stacking order and changes on
/// every activation; Mutter ids only grow, so sorting by id gives the
/// stable creation order the backend contract requires.
fn parse_all(json: &str) -> Result<Vec<GnomeWindow>, BackendError> {
    let mut all: Vec<GnomeWindow> = serde_json::from_str(json)
        .map_err(|e| BackendError::CommandFailed(format!("bad Window Calls JSON: {e}")))?;
    all.sort_by_key(|w| w.id);
    Ok(all)
}

fn snapshot_from(all: &[GnomeWindow], class: &WindowClass) -> WindowSnapshot {
    let windows: Vec<WindowId> = all
        .iter()
        .filter(|w| w.matches(class))
        .map(GnomeWindow::window_id)
        .collect();
    let active = all.iter().find(|w| w.focus).map(GnomeWindow::window_id);
    WindowSnapshot { windows, active }
}

/// One class per window that has one: `wm_class`, else the instance name.
fn classes_from(all: &[GnomeWindow]) -> Vec<String> {
    all.iter()
        .filter_map(|w| w.wm_class.clone().or_else(|| w.wm_class_instance.clone()))
        .filter(|c| !c.is_empty())
        .collect()
}

/// The title of each of `ids`, in order; `""` for a window not in `all`.
fn titles_from(all: &[GnomeWindow], ids: &[WindowId]) -> Vec<String> {
    ids.iter()
        .map(|id| {
            all.iter()
                .find(|w| w.window_id() == *id)
                .and_then(|w| w.title.clone())
                .unwrap_or_default()
        })
        .collect()
}

/// The `u32` that Window Calls' `Activate`/`Minimize`/`Close` take. Ids are
/// listed as `u64` (one large id must not fail the whole `List` parse), so an
/// id past `u32::MAX` is reported as out of range rather than malformed.
fn parse_id(id: &WindowId) -> Result<u32, BackendError> {
    let wide: u64 = id
        .as_str()
        .parse()
        .map_err(|_| BackendError::CommandFailed(format!("not a Window Calls id: {id}")))?;
    u32::try_from(wide).map_err(|_| {
        BackendError::CommandFailed(format!(
            "Window Calls id {id} is out of range for its u32 methods"
        ))
    })
}

/// D-Bus error names meaning the extension (or GNOME Shell) isn't there, as
/// opposed to a call that reached it and failed (e.g. a window that closed
/// between `List` and `Activate`).
fn is_unavailable_error_name(name: &str) -> bool {
    matches!(
        name,
        "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner"
            | "org.freedesktop.DBus.Error.UnknownObject"
            | "org.freedesktop.DBus.Error.UnknownInterface"
            | "org.freedesktop.DBus.Error.UnknownMethod"
    )
}

fn classify_call_error(e: &zbus::Error) -> BackendError {
    match e {
        zbus::Error::MethodError(name, _, _) if is_unavailable_error_name(name.as_str()) => {
            BackendError::Unavailable(format!(
                "Window Calls GNOME Shell extension not available: {e}"
            ))
        }
        _ => BackendError::CommandFailed(format!("Window Calls call failed: {e}")),
    }
}

#[async_trait]
impl WindowBackend for GnomeWindowCallsBackend {
    async fn list_windows(&self, class: &WindowClass) -> Result<Vec<WindowId>, BackendError> {
        Ok(snapshot_from(&self.list().await?, class).windows)
    }

    async fn open_window_classes(&self) -> Result<Vec<String>, BackendError> {
        Ok(classes_from(&self.list().await?))
    }

    async fn activate(&self, id: &WindowId) -> Result<(), BackendError> {
        self.call("Activate", &parse_id(id)?).await.map(drop)
    }

    async fn minimize(&self, id: &WindowId) -> Result<(), BackendError> {
        self.call("Minimize", &parse_id(id)?).await.map(drop)
    }

    async fn close(&self, id: &WindowId) -> Result<(), BackendError> {
        self.call("Close", &parse_id(id)?).await.map(drop)
    }

    async fn active_window(&self) -> Result<Option<WindowId>, BackendError> {
        Ok(self
            .list()
            .await?
            .iter()
            .find(|w| w.focus)
            .map(GnomeWindow::window_id))
    }

    /// One more `List` call.
    async fn window_titles(&self, ids: &[WindowId]) -> Result<Option<Vec<String>>, BackendError> {
        Ok(Some(titles_from(&self.list().await?, ids)))
    }

    /// One `List` call answers both questions.
    async fn snapshot(&self, class: &WindowClass) -> Result<WindowSnapshot, BackendError> {
        Ok(snapshot_from(&self.list().await?, class))
    }
}

/// A Window Calls `List` reply with one window of `window_class` (class
/// and instance), run through the real parser and matcher.
#[cfg(test)]
pub(super) fn near_miss_probe(window_class: &str, wanted: &WindowClass) -> bool {
    let json = format!(
        r#"[{{"id": 1, "wm_class": "{window_class}", "wm_class_instance": "{window_class}", "focus": false}}]"#
    );
    !snapshot_from(&parse_all(&json).unwrap(), wanted)
        .windows
        .is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Not captured live (no GNOME session was available). The shape follows
    // the Window Calls extension's own `List()` implementation upstream
    // (github.com/ickyicky/window-calls, extension.js): every window gets
    // wm_class, wm_class_instance, title, pid, id, frame_type, window_type,
    // width, height, x, y, focus, in_current_workspace and workspace, listed
    // in stacking order (focused window last).
    const SAMPLE: &str = include_str!("../../tests/fixtures/window-calls-list.json");

    #[test]
    fn every_classed_window_offers_a_class_that_matches_it() {
        let all = parse_all(SAMPLE).unwrap();
        let classes = classes_from(&all);
        assert!(!classes.is_empty());
        // The class-less panel offers nothing.
        assert!(classes.len() < all.len());
        for c in &classes {
            assert!(
                !snapshot_from(&all, &class(c)).windows.is_empty(),
                "{c} matches no window"
            );
        }
    }

    fn class(s: &str) -> WindowClass {
        WindowClass::parse(s).unwrap()
    }

    fn ids(list: &[&str]) -> Vec<WindowId> {
        list.iter().map(|s| WindowId::new(*s)).collect()
    }

    #[test]
    fn matches_exactly_and_case_insensitively_in_stable_id_order() {
        let all = parse_all(SAMPLE).unwrap();
        let snap = snapshot_from(&all, &class("firefox"));
        // Ids 1904 and 2210 are Firefox; 1983 is firefox-esr and must not
        // match. The reply lists 2210 before 1904 (stacking order).
        assert_eq!(snap.windows, ids(&["1904", "2210"]));
    }

    #[test]
    fn reads_titles_in_the_order_asked_and_empty_for_a_gone_window() {
        let all = parse_all(SAMPLE).unwrap();
        assert_eq!(
            titles_from(&all, &ids(&["1904", "2210", "9999", "1022"])),
            vec!["Private Browsing", "Mozilla Firefox", "", ""]
        );
    }

    #[test]
    fn a_missing_or_null_title_is_empty() {
        let all = parse_all(
            r#"[{"id": 1, "wm_class": "a", "focus": false},
                {"id": 2, "wm_class": "a", "title": null, "focus": false}]"#,
        )
        .unwrap();
        assert_eq!(titles_from(&all, &ids(&["1", "2"])), vec!["", ""]);
    }

    #[test]
    fn matches_the_instance_half_too() {
        let all = parse_all(SAMPLE).unwrap();
        // Every Firefox flavour shares the `Navigator` instance name.
        assert_eq!(
            snapshot_from(&all, &class("navigator")).windows,
            ids(&["1904", "1983", "2210"])
        );
    }

    #[test]
    fn reports_the_focused_window_from_the_same_reply() {
        let all = parse_all(SAMPLE).unwrap();
        let snap = snapshot_from(&all, &class("org.gnome.Nautilus"));
        assert_eq!(snap.windows, ids(&["2044"]));
        assert_eq!(snap.active, Some(WindowId::new("2044")));
    }

    #[test]
    fn windows_with_a_null_class_never_match() {
        let all = parse_all(SAMPLE).unwrap();
        assert!(all.iter().any(|w| w.wm_class.is_none()));
        assert!(snapshot_from(&all, &class("null")).windows.is_empty());
    }

    #[test]
    fn an_id_beyond_u32_is_out_of_range_not_malformed() {
        let err = parse_id(&WindowId::new("4294967296")).unwrap_err();
        assert!(err.to_string().contains("out of range"), "{err}");
        let err = parse_id(&WindowId::new("abc")).unwrap_err();
        assert!(err.to_string().contains("not a Window Calls id"), "{err}");
        assert_eq!(parse_id(&WindowId::new("42")).unwrap(), 42);
        assert_eq!(parse_id(&WindowId::new("4294967295")).unwrap(), u32::MAX);
    }

    #[test]
    fn a_reply_without_focus_is_rejected_rather_than_assumed_unfocused() {
        let result = parse_all(r#"[{"id": 1, "wm_class": "firefox"}]"#);
        assert!(matches!(result, Err(BackendError::CommandFailed(_))));
    }

    #[test]
    fn bad_json_is_a_command_failed_error() {
        assert!(matches!(
            parse_all("not json"),
            Err(BackendError::CommandFailed(_))
        ));
    }

    #[test]
    fn parses_ids_and_rejects_foreign_ones() {
        assert_eq!(parse_id(&WindowId::new("2044")).unwrap(), 2044);
        assert!(parse_id(&WindowId::new("{uuid}")).is_err());
    }

    #[test]
    fn only_missing_service_errors_count_as_unavailable() {
        assert!(is_unavailable_error_name(
            "org.freedesktop.DBus.Error.ServiceUnknown"
        ));
        assert!(is_unavailable_error_name(
            "org.freedesktop.DBus.Error.UnknownObject"
        ));
        assert!(!is_unavailable_error_name(
            "org.freedesktop.DBus.Error.Failed"
        ));
        assert!(!is_unavailable_error_name("org.gnome.gjs.JSError.Error"));
    }
}
