// Hides the console window of a release build on Windows, which Procly does not target yet.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    procly_desktop_lib::run();
}
