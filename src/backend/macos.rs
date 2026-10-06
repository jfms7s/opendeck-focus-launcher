//! macOS window backend: the Accessibility API (AX). Running instances of
//! an app are found from the kernel's process list (always current; AppKit's
//! `NSWorkspace` lists only update while a main run loop runs, which this
//! plugin doesn't have). Every AX call blocks, so each operation runs on
//! `spawn_blocking`, with a 1 s timeout per app so a hung app can't stall a
//! key press.
//!
//! AX needs the Accessibility permission for the app that started the
//! plugin (OpenDeck). Without it, launching still works: an app with no
//! running instance simply has no windows.

use super::mac_ids::{bundle_of, ordered, parse_window_id, window_id};
use super::{BackendError, WindowBackend, WindowClass, WindowId};
use async_trait::async_trait;
use objc2_application_services::{
    AXError, AXIsProcessTrusted, AXIsProcessTrustedWithOptions, AXUIElement,
};
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::{Mutex, Once};

const AX_TIMEOUT_SECS: f32 = 1.0;
const NEEDS_PERMISSION: &str =
    "allow OpenDeck in System Settings > Privacy & Security > Accessibility";

// Private, but stable for over a decade and what yabai and AltTab use to
// get a window's CGWindowID from its AX element.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn _AXUIElementGetWindow(element: &AXUIElement, window: *mut u32) -> AXError;
}

fn attr(name: &'static str) -> CFRetained<CFString> {
    CFString::from_static_str(name)
}

fn failed(what: &str, err: AXError) -> BackendError {
    BackendError::CommandFailed(format!("{what} (AXError {})", err.0))
}

/// An attribute's value, if the element has it.
fn copy(element: &AXUIElement, name: &'static str) -> Option<CFRetained<CFType>> {
    let mut value: *const CFType = std::ptr::null();
    // SAFETY: `value` is a valid out-pointer; on success it holds a +1
    // reference, which `from_raw` takes ownership of.
    let err = unsafe { element.copy_attribute_value(&attr(name), NonNull::from(&mut value)) };
    if err != AXError::Success {
        return None;
    }
    NonNull::new(value.cast_mut()).map(|v| unsafe { CFRetained::from_raw(v) })
}

fn copy_element(element: &AXUIElement, name: &'static str) -> Option<CFRetained<AXUIElement>> {
    copy(element, name)?.downcast::<AXUIElement>().ok()
}

fn copy_string(element: &AXUIElement, name: &'static str) -> Option<String> {
    copy(element, name)?
        .downcast::<CFString>()
        .ok()
        .map(|s| s.to_string())
}

fn set_bool(element: &AXUIElement, name: &'static str, value: bool) -> Result<(), AXError> {
    // SAFETY: both CF values are valid for the call.
    let err = unsafe { element.set_attribute_value(&attr(name), CFBoolean::new(value)) };
    if err == AXError::Success {
        Ok(())
    } else {
        Err(err)
    }
}

fn perform(element: &AXUIElement, action: &'static str) -> Result<(), AXError> {
    // SAFETY: `element` and the action name are valid CF objects.
    let err = unsafe { element.perform_action(&attr(action)) };
    if err == AXError::Success {
        Ok(())
    } else {
        Err(err)
    }
}

/// Sets the AX messaging timeout for the whole process (what the
/// system-wide element's timeout means), so window elements, buttons and
/// everything else copied from an app are bounded too, not only the app
/// elements given their own timeout. Done once, before any AX call.
fn bound_every_ax_call() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: creating the system-wide element has no preconditions, and
        // the timeout is a plain float.
        unsafe { AXUIElement::new_system_wide().set_messaging_timeout(AX_TIMEOUT_SECS) };
    });
}

fn app_element(pid: i32) -> CFRetained<AXUIElement> {
    bound_every_ax_call();
    // SAFETY: creating an AX reference for a pid has no preconditions.
    let app = unsafe { AXUIElement::new_application(pid) };
    // SAFETY: `app` is a valid element; the timeout is a plain float.
    unsafe { app.set_messaging_timeout(AX_TIMEOUT_SECS) };
    app
}

fn window_number(window: &AXUIElement) -> Option<u32> {
    let mut number = 0u32;
    // SAFETY: `window` is a valid AX element and `number` a valid out-pointer.
    let err = unsafe { _AXUIElementGetWindow(window, &mut number) };
    (err == AXError::Success && number != 0).then_some(number)
}

/// An app's standard windows (minimised ones included) with their numbers.
fn standard_windows(app: &AXUIElement) -> Vec<(u32, CFRetained<AXUIElement>)> {
    let Some(list) = copy(app, "AXWindows").and_then(|v| v.downcast::<CFArray>().ok()) else {
        return Vec::new();
    };
    // SAFETY: AXWindows is documented as an array of AXUIElements.
    let list: &CFArray<AXUIElement> = unsafe { list.cast_unchecked() };
    list.iter()
        .filter(|w| copy_string(w, "AXSubrole").as_deref() == Some("AXStandardWindow"))
        .filter_map(|w| window_number(&w).map(|n| (n, w)))
        .collect()
}

/// Every running process as `(pid, executable path)`.
fn processes() -> Vec<(i32, PathBuf)> {
    // SAFETY: a null buffer asks for the number of pids.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Vec::new();
    }
    // Room for processes started between the two calls.
    let mut pids = vec![0 as libc::pid_t; count as usize + 64];
    let bytes = (pids.len() * size_of::<libc::pid_t>()) as libc::c_int;
    // SAFETY: `pids` holds `bytes` bytes.
    let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(n.max(0) as usize);
    pids.into_iter()
        .filter(|&pid| pid > 0)
        .filter_map(|pid| executable(pid).map(|p| (pid, p)))
        .collect()
}

fn executable(pid: i32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buf` is valid for `buf.len()` bytes.
    let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (len > 0).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len as usize])))
}

/// `AXIsProcessTrusted`, asking macOS to show its permission prompt the
/// first time it says no.
fn trusted() -> bool {
    // SAFETY: no arguments.
    if unsafe { AXIsProcessTrusted() } {
        return true;
    }
    static PROMPT: Once = Once::new();
    PROMPT.call_once(|| {
        let key = attr("AXTrustedCheckOptionPrompt");
        let options =
            CFDictionary::<CFString, CFBoolean>::from_slices(&[&key], &[CFBoolean::new(true)]);
        // SAFETY: a valid options dictionary.
        unsafe { AXIsProcessTrustedWithOptions(Some(options.as_opaque())) };
        log::warn!("the Accessibility permission is missing: {NEEDS_PERMISSION}");
    });
    false
}

/// Window control for macOS apps, matched by bundle id.
#[derive(Default)]
pub struct MacAccessibilityBackend {
    /// Bundle id per bundle path (`None`: not an app with a bundle id).
    bundle_ids: Mutex<HashMap<PathBuf, Option<String>>>,
}

impl MacAccessibilityBackend {
    fn bundle_id(&self, bundle: &Path) -> Option<String> {
        let mut cache = self
            .bundle_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache
            .entry(bundle.to_path_buf())
            .or_insert_with(|| crate::bundle::read_bundle(bundle).map(|a| a.id))
            .clone()
    }

    /// The pids of every running instance of `class` (a bundle id).
    fn pids(&self, class: &WindowClass) -> Vec<i32> {
        processes()
            .into_iter()
            .filter(|(_, exe)| {
                bundle_of(exe)
                    .and_then(|b| self.bundle_id(&b))
                    .is_some_and(|id| class.matches(&id))
            })
            .map(|(pid, _)| pid)
            .collect()
    }

    fn list_blocking(&self, class: &WindowClass) -> Result<Vec<WindowId>, BackendError> {
        let pids = self.pids(class);
        if pids.is_empty() {
            return Ok(Vec::new());
        }
        if !trusted() {
            return Err(BackendError::Unavailable(NEEDS_PERMISSION.to_string()));
        }
        let windows = pids
            .into_iter()
            .flat_map(|pid| {
                standard_windows(&app_element(pid))
                    .into_iter()
                    .map(move |(n, _)| (n, pid))
            })
            .collect();
        Ok(ordered(windows))
    }
}

/// The app and window an id names, if both still exist.
fn find(id: &WindowId) -> Result<(CFRetained<AXUIElement>, CFRetained<AXUIElement>), BackendError> {
    let (pid, number) = parse_window_id(id)
        .ok_or_else(|| BackendError::CommandFailed(format!("not a macOS window id: {id}")))?;
    if !trusted() {
        return Err(BackendError::Unavailable(NEEDS_PERMISSION.to_string()));
    }
    let app = app_element(pid);
    let window = standard_windows(&app)
        .into_iter()
        .find(|(n, _)| *n == number)
        .map(|(_, w)| w)
        .ok_or_else(|| BackendError::CommandFailed(format!("window {id} is gone")))?;
    Ok((app, window))
}

/// Brings one window to the front, as a click on it would: un-minimise it,
/// make it the app's main window (so the app activates on it, and cycling
/// sees it as focused next time), un-hide the app, bring the app forward,
/// then raise the window. Only bringing the app forward must succeed; the
/// rest isn't supported by every window and is best effort.
fn activate_blocking(id: &WindowId) -> Result<(), BackendError> {
    let (app, window) = find(id)?;
    let _ = set_bool(&window, "AXMinimized", false);
    let _ = set_bool(&window, "AXMain", true);
    let _ = set_bool(&app, "AXHidden", false);
    set_bool(&app, "AXFrontmost", true).map_err(|e| failed("bringing the app forward", e))?;
    if let Err(e) = perform(&window, "AXRaise") {
        log::warn!(
            "window {id} could not be raised (AXError {}); its app is in front",
            e.0
        );
    }
    Ok(())
}

fn minimize_blocking(id: &WindowId) -> Result<(), BackendError> {
    let (_, window) = find(id)?;
    set_bool(&window, "AXMinimized", true).map_err(|e| failed("minimising the window", e))
}

fn close_blocking(id: &WindowId) -> Result<(), BackendError> {
    let (_, window) = find(id)?;
    let button = copy_element(&window, "AXCloseButton")
        .ok_or_else(|| BackendError::CommandFailed(format!("window {id} has no close button")))?;
    perform(&button, "AXPress").map_err(|e| failed("closing the window", e))
}

fn active_blocking() -> Result<Option<WindowId>, BackendError> {
    // SAFETY: no arguments; querying is harmless without the permission.
    if !unsafe { AXIsProcessTrusted() } {
        return Ok(None);
    }
    bound_every_ax_call();
    // SAFETY: creating the system-wide element has no preconditions.
    let system = unsafe { AXUIElement::new_system_wide() };
    let Some(app) = copy_element(&system, "AXFocusedApplication") else {
        return Ok(None);
    };
    let mut pid: libc::pid_t = 0;
    // SAFETY: `pid` is a valid out-pointer.
    if unsafe { app.pid(NonNull::from(&mut pid)) } != AXError::Success {
        return Ok(None);
    }
    Ok(copy_element(&app, "AXFocusedWindow")
        .and_then(|w| window_number(&w))
        .map(|n| window_id(pid, n)))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, BackendError> + Send + 'static,
) -> Result<T, BackendError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| BackendError::CommandFailed(format!("window task failed: {e}")))?
}

#[async_trait]
impl WindowBackend for std::sync::Arc<MacAccessibilityBackend> {
    async fn list_windows(&self, class: &WindowClass) -> Result<Vec<WindowId>, BackendError> {
        let this = self.clone();
        let class = class.clone();
        blocking(move || this.list_blocking(&class)).await
    }

    async fn activate(&self, id: &WindowId) -> Result<(), BackendError> {
        let id = id.clone();
        blocking(move || activate_blocking(&id)).await
    }

    async fn minimize(&self, id: &WindowId) -> Result<(), BackendError> {
        let id = id.clone();
        blocking(move || minimize_blocking(&id)).await
    }

    async fn close(&self, id: &WindowId) -> Result<(), BackendError> {
        let id = id.clone();
        blocking(move || close_blocking(&id)).await
    }

    async fn active_window(&self) -> Result<Option<WindowId>, BackendError> {
        blocking(active_blocking).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Runs on the macOS CI runner: an app with no running instance has no
    /// windows, with or without the Accessibility permission (so a key
    /// press launches it).
    #[tokio::test]
    async fn an_app_that_is_not_running_has_no_windows() {
        let backend = Arc::new(MacAccessibilityBackend::default());
        let class = WindowClass::parse("org.example.not-running").unwrap();
        assert_eq!(backend.list_windows(&class).await, Ok(Vec::new()));
    }

    #[test]
    fn running_processes_are_listed() {
        let me = std::process::id() as i32;
        assert!(processes().iter().any(|(pid, _)| *pid == me));
    }

    #[test]
    fn the_system_apps_are_discovered() {
        let apps = crate::apps::list_installed_apps();
        assert!(
            apps.iter()
                .any(|a| a.id.eq_ignore_ascii_case("com.apple.calculator")),
            "{} apps, no Calculator",
            apps.len()
        );
    }
}
