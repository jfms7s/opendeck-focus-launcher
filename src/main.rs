mod action;
mod apps;
mod backend;
// macOS discovery; tested on every platform.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod bundle;
mod catalog;
mod decision;
mod icon;
mod orchestrate;
mod settings;

use action::FocusOrLaunchAction;
use backend::{BackendKind, select_backend};
use openaction::{OpenActionResult, register_action, run};

// Two workers are plenty: handlers are short, and every blocking step (the
// apps scan, icon lookups and reads) runs on the blocking pool instead.
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> OpenActionResult<()> {
    simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default())
        .expect("logger init");

    let kind = select_backend(
        std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
        std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
        std::env::var("DISPLAY").ok().as_deref(),
    );
    match kind {
        Some(kind) => log::info!("using the {} window backend", kind.name()),
        None => log::error!(
            "no supported window backend for this desktop session (KDE Plasma, GNOME Shell \
             or an X11 session are supported); Focus or Launch keys will alert and do nothing"
        ),
    }

    register_action(FocusOrLaunchAction::new(kind.map(BackendKind::build))).await;
    run(std::env::args().collect()).await
}
