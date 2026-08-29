// Hide the console window in release; keep it in debug for engine logging.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    geminiflow_lib::run()
}
