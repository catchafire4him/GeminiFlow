mod audio;
mod commands;
mod engine;
mod gemini;
mod hotkey;
mod inject;
mod logging;
mod mic;
mod overlay;
mod control;
mod secrets;
mod sound;
mod settings;
mod startup;
mod touch;
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

    // The configuration every later line has to be read against. Without
    // it a log full of batch dictations is ambiguous: it could mean live
    // failed every time, or that live was simply switched off.
    crate::logln!(
        "[config] live streaming {}, language {}, paste {}, speakerphone {}, \
         keep dictation audio {}, external control {}, verbose {}",
        if settings.use_live { "on" } else { "off" },
        settings.language,
        settings.paste_mode,
        if settings.speakerphone { "on" } else { "off" },
        if settings.keep_dictation_audio { "on" } else { "off" },
        if settings.control_enabled { "on" } else { "off" },
        if settings.debug_logging { "on" } else { "off" },
    );

    // Aged-out dictation audio goes at launch rather than on a timer: the
    // app runs for days at a stretch, but it is also started often enough
    // that nothing lingers far past its window.
    let pruned = store.prune_dictation_audio(settings.dictation_audio_days);
    if pruned > 0 {
        crate::logln!("[store] pruned {pruned} expired dictation recordings");
    }

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
            commands::touch_press,
            commands::touch_release,
            commands::touch_cancel,
            commands::touch_move,
            commands::touch_dragging,
            commands::touch_settle,
            commands::touch_dismiss,
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

            // After the overlay, since it borrows that module's window
            // styling. The saved position is only a hint -- it is clamped
            // to whatever screen actually exists now.
            {
                let s = state.settings.lock();
                let (on, size, saved) = match s {
                    Ok(s) => (
                        s.touch_button,
                        s.touch_size,
                        if s.touch_placed { Some((s.touch_x, s.touch_y)) } else { None },
                    ),
                    Err(_) => (false, 88.0, None),
                };
                touch::setup(app.handle(), on, size, saved);
            }

            // The keyboard hook needs its own thread with a message pump, and
            // the engine needs a thread that is free to block on the network.
            let (tx, rx) = channel::<hotkey::Event>();
            engine::spawn(app.handle().clone(), Arc::clone(&state), rx);

            // After the engine, so a client connecting immediately finds a
            // state to read rather than a half-built one.
            if state.settings.lock().map(|s| s.control_enabled).unwrap_or(false) {
                let port = state
                    .settings
                    .lock()
                    .map(|s| s.control_port)
                    .unwrap_or(8787) as u16;
                if let Err(e) = control::start(Arc::clone(&state), port) {
                    crate::logln!("[control] could not start: {e:#}");
                }
            }

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
    // The way back after dragging the button onto the dismiss target.
    // Without this it could be put away and never retrieved except by
    // hunting through Settings.
    let touch = MenuItem::with_id(app, "touch", "Show touch button", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &touch, &quit])?;

    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("GeminiFlow")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "touch" => reveal_touch_button(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;

    Ok(())
}

/// Brings the floating button back and remembers that it is wanted.
fn reveal_touch_button(app: &tauri::AppHandle) {
    use tauri::Manager;
    let Some(state) = app.try_state::<Arc<AppState>>() else { return };

    let (size, saved) = match state.settings.lock() {
        Ok(mut s) => {
            s.touch_button = true;
            let _ = s.save(&state.store);
            (
                s.touch_size,
                if s.touch_placed { Some((s.touch_x, s.touch_y)) } else { None },
            )
        }
        Err(_) => (88.0, None),
    };

    // Re-placed as well as shown. If it was dismissed off the edge of a
    // screen that no longer exists, simply showing it again would put it
    // somewhere unreachable.
    touch::place(app, saved, size);
    touch::show(app);
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
