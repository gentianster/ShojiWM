//! Run with `cargo run -p xdg-desktop-portal-shojiwm --example picker_demo`.
//! This standalone window uses synthetic sources and never connects to D-Bus
//! or captures the screen. Cancel, Escape and closing the window exit it.
#![allow(dead_code)]
#[cfg(debug_assertions)]
#[path = "../src/i18n/mod.rs"]
mod i18n;
#[cfg(debug_assertions)]
#[path = "../src/picker.rs"]
mod picker;
#[cfg(debug_assertions)]
#[path = "../src/sources.rs"]
mod sources;

#[cfg(debug_assertions)]
fn main() -> iced::Result {
    picker::run_demo()
}

#[cfg(not(debug_assertions))]
fn main() {
    eprintln!("The picker demo is only available in debug builds.");
}
