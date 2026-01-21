mod config;
mod controller;
mod models;
mod mpv;
mod radio_browser;
mod ui;

use tracing_subscriber::EnvFilter;

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .init();

    cosmic::applet::run::<ui::RadioWidget>(())
}
