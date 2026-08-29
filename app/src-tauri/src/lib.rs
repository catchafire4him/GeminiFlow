mod audio;
mod commands;
mod engine;
mod gemini;
mod hotkey;
mod inject;
mod logging;
mod mic;
mod overlay;
mod secrets;
mod sound;
mod settings;
mod startup;
mod store;

use std::sync::mpsc::channel;
use std::sync::Arc;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, WindowEvent};

use engine::AppState;
use settings::Settings;
use store::Store;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    logging::start_session();

    let store = Arc::new(Store::open().expect("could not open the GeminiFlow database"));

    let settings = Settings::load(&store);
    logging::set_debug(settings.debug_logging);
    sound::set_enabled(settings.sounds_enabled);
    sound::set_volume(settings.sound_volume);

    // Keeps the Run entry pointing at wherever the app now lives; a moved or
    // reinstalled executable would otherwise silently stop starting.
    startup::refresh_if_enabled();
    hotkey::set_binding(&settings.hotkey);
    hotkey::set_notes_binding(&settings.notes_hotkey);
    hotkey::set_call_binding(&settings.call_hotkey);

    match store.prune(settings.retention_days) {
        Ok(n) if n > 0 => logln!("pruned {n} dictations past the retention window"),
        Err(e) => logln!("retention sweep failed: {e}"),
        _ => {}
    }

    let state = Arc::new(AppState::new(Arc::clone(&store), settings));

    tauri::Builder::default()
        .manage(Arc::clone(&state))
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::get_settings,
            commands::save_settings,
            commands::list_input_devices,
            commands::get_vocabulary,
            commands::save_vocabulary,
            commands::list_dictations,
            commands::delete_dictation,
            commands::has_api_key,
            commands::set_api_key,
            commands::clear_api_key,
            commands::list_notes,
            commands::get_note,
            commands::set_action_done,
            commands::delete_note,
            commands::send_to_vscode,
            commands::ui_hotkey,
            commands::retry_note_summary,
            commands::data_stats,
            commands::open_data_folder,
            commands::recover_recordings,
            commands::orphaned_audio,
            commands::note_audio,
            commands::read_log,
            commands::test_sound,
            commands::mic_level,
            commands::set_mic_level,
            commands::set_mic_muted,
            commands::delete_orphaned_audio,
            commands::clear_dictations,
            commands::delete_all_notes,
        ])
        .setup(move |app| {
            build_tray(app.handle())?;

            // A login launch goes straight to the tray rather than throwing a
            // window up on every boot.
            let hidden = startup::launched_hidden()
                || state.settings.lock().map(|s| s.start_hidden).unwrap_or(false);
            if hidden {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
                logln!("started hidden to the tray");
            }

            overlay::setup(app.handle());

            // The keyboard hook needs its own thread with a message pump, and
            // the engine needs a thread that is free to block on the network.
            let (tx, rx) = channel::<hotkey::Event>();
            engine::spawn(app.handle().clone(), Arc::clone(&state), rx);

            std::thread::spawn(move || {
                if let Err(e) = hotkey::install_and_pump(tx) {
                    // Goes to the log file, not just the console: a hook that
                    // fails to install kills every shortcut in the app, and
                    // the symptom is silence.
                    crate::logln!("[hotkey] FAILED to install: {e:#}");
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing hides to the tray. This is a background tool; quitting
            // it by accident silently kills the hotkey.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running GeminiFlow");
}

fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Open GeminiFlow", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("GeminiFlow")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;

    Ok(())
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
