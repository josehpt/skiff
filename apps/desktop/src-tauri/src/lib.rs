//! The desktop app is a client of `skiffd`. It owns no PTY.

mod cli_path;
mod clipboard;
mod connection;
mod commands;
mod config;
mod diagnostics;
mod input;
mod reload;
mod skiff_agent;
mod streaming;
mod native_menu;
#[cfg(target_os = "linux")]
mod header_bar;

use connection::App;
use tauri::{Emitter, Manager};

/// The renderer switch Skiff sets for its own webview. Sessions must not
/// inherit it: it slows every WebKitGTK app started from a Skiff shell.
pub(crate) const WEBKIT_DMABUF_VAR: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";

/// True when Skiff set `WEBKIT_DMABUF_VAR` itself, not the user.
pub(crate) static SET_WEBKIT_DMABUF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// WebKitGTK paints a blank window on NVIDIA under Wayland when it renders
/// through DMA-BUF. Turn that path off there, unless the user chose
/// otherwise. X11, Intel and AMD keep it: it is the faster path.
#[cfg(target_os = "linux")]
fn disable_dmabuf_where_broken() {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var("GDK_BACKEND").map_or(true, |b| !b.starts_with("x11"));
    if std::env::var_os(WEBKIT_DMABUF_VAR).is_none()
        && wayland
        && std::path::Path::new("/proc/driver/nvidia").exists()
    {
        std::env::set_var(WEBKIT_DMABUF_VAR, "1");
        SET_WEBKIT_DMABUF.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "linux")]
    disable_dmabuf_where_broken();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(App::default())
        // The window starts hidden: GTK takes a header bar only before it shows.
        .setup(|app| {
            std::thread::spawn(cli_path::sync);
            std::thread::spawn(skiff_agent::sync);
            let Some(window) = app.get_webview_window("main") else { return Ok(()) };
            #[cfg(target_os = "linux")]
            if header_bar::wanted() {
                header_bar::install(app.handle(), &window)?;
            }
            window.show()?;
            Ok(())
        })
        .on_menu_event(|app, event| {
            let _ = app.emit("skiff:menu", event.id().as_ref());
        })
        .invoke_handler(tauri::generate_handler![
            native_menu::set_menu,
            native_menu::quit_app,
            native_menu::menu_layout,
            native_menu::set_menu_layout,
            native_menu::set_window_dark,
            clipboard::clipboard_write,
            clipboard::clipboard_read,
            connection::daemon_status,
            cli_path::install_cli,
            skiff_agent::get_skiff_agent,
            skiff_agent::set_skiff_agent,
            skiff_agent::start_skiff_watch,
            skiff_agent::skiff_watch_log,
            skiff_agent::open_skiff_agent,
            commands::list_sessions,
            commands::create_session,
            commands::pty_write,
            commands::pty_resize,
            commands::session_seen,
            commands::kill_session,
            commands::pty_submit,
            commands::rename_session,
            commands::set_session_theme,
            config::list_projects,
            config::get_project,
            config::add_worktree,
            config::remove_worktree,
            config::add_project,
            connection::restart_daemon,
            reload::reload_daemon,
            config::inspect_folder,
            config::init_repository,
            config::set_project_icon,
            config::set_project_short,
            config::set_project_fleet,
            config::set_project_color,
            config::set_project_closed,
            config::remove_project,
            config::set_project_background,
            config::set_project_background_opacity,
            commands::read_image,
            commands::list_dir,
            commands::git_changes,
            commands::open_diff,
            commands::open_settings,
            commands::set_open,
            commands::set_open_peek,
            commands::preview_file,
            commands::open_file,
            commands::reveal_file,
            commands::open_path,
            commands::open_url,
            commands::files_projects,
            commands::hidden_changes_projects,
            commands::set_project_changes,
            commands::set_project_files,
            commands::project_themes,
            commands::set_project_theme,
            config::reorder_projects,
            commands::list_themes,
            commands::import_themes,
            commands::open_theme_folder,
            config::get_appearance,
            diagnostics::memory_usage,
            config::get_keys,
            commands::config_path,
            diagnostics::trace_info,
            diagnostics::trace_write,
            diagnostics::trace_set,
            diagnostics::system_load,
            commands::open_config,
            config::set_appearance,
            config::set_font_size,
            commands::list_agents,
            commands::list_agent_sessions,
            config::set_agents,
            config::set_agent_command,
            config::rename_agent,
            config::remove_agent,
            config::restore_agents,
            commands::list_groups,
            commands::session_screen,
            commands::save_group,
            commands::delete_group,
            streaming::subscribe_output,
            streaming::ack_output,
            streaming::unsubscribe_output,
            streaming::subscribe_events,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
