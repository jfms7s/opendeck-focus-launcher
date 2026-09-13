mod action;
mod apps;
mod backend;
mod decision;

use action::FocusOrLaunchAction;
use openaction::{register_action, run, OpenActionResult};

#[tokio::main]
async fn main() -> OpenActionResult<()> {
    simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default())
        .expect("logger init");
    register_action(FocusOrLaunchAction::new()).await;
    run(std::env::args().collect()).await
}
