mod action;
mod apps;
mod backend;
mod decision;

use action::FocusOrLaunchAction;
use openaction::{OpenActionResult, register_action, run};

#[tokio::main]
async fn main() -> OpenActionResult<()> {
    simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default())
        .expect("logger init");
    register_action(FocusOrLaunchAction::new()).await;
    run(std::env::args().collect()).await
}
