// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Lakko desktop host (ADR-0085).
//!
//! The window loads the bundled `apps/web` build from the app's own URI
//! scheme. The same scheme handler forwards every relative `/api/<service>/`
//! request to one configured Lakko origin, so the web client's `fetch` calls
//! run unchanged and nothing crosses an origin; it also answers the host's
//! own routes (`/__host/device-key/*`, `/__host/app/*`).
//!
//! `LAKKO_DESKTOP_PROBE=1` opens the measurement page of `probe.rs` instead
//! of the app and hides the window for a while, to see what a hidden webview
//! keeps doing.

#![windows_subsystem = "windows"]

mod app_shell;
mod device_key;
mod key_store;
mod notify;
mod probe;
mod proxy;
mod tunnel;
#[cfg(not(target_os = "macos"))]
mod tray;

use std::sync::Arc;

use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tracing::{error, info, warn};

use app_shell::{CloseAction, Shell, MAIN};

/// The URI scheme the window's page is served from.
pub const APP_SCHEME: &str = "lakkoapp";

/// Where the page lives, as each OS's webview spells a custom scheme.
fn app_url(path: &str) -> String {
    if cfg!(windows) {
        format!("http://{APP_SCHEME}.localhost{path}")
    } else {
        format!("{APP_SCHEME}://localhost{path}")
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let config = match proxy::ProxyConfig::from_env() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            error!(error = %e, "invalid proxy configuration");
            std::process::exit(2);
        },
    };
    let probe_mode = std::env::var("LAKKO_DESKTOP_PROBE").is_ok_and(|v| v == "1");
    let start_hidden = std::env::args().any(|a| a == app_shell::HIDDEN_ARG);
    info!(api_origin = %config.api_origin, probe_mode, start_hidden, "lakko desktop host starting");

    let handler_config = Arc::clone(&config);
    let setup_config = Arc::clone(&config);
    let built = tauri::Builder::default()
        .enable_macos_default_menu(false)
        .register_asynchronous_uri_scheme_protocol(APP_SCHEME, move |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            let config = Arc::clone(&handler_config);
            tauri::async_runtime::spawn(async move {
                let response = proxy::handle(&app, &config, request).await;
                responder.respond(response);
            });
        })
        .on_menu_event(|app, event| app_shell::on_menu(app, event.id().as_ref()))
        .on_window_event(|window, event| {
            if window.label() != MAIN {
                return;
            }
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                let Some(shell) = app.try_state::<Shell>() else { return };
                let Some(w) = app.get_webview_window(MAIN) else { return };
                app_shell::remember_geometry(&w, &shell);
                api.prevent_close();
                match app_shell::on_close_requested(&shell) {
                    CloseAction::Hide => {
                        let _ = w.hide();
                        app_shell::after_hide(app, &shell);
                    }
                    CloseAction::Minimize => {
                        let _ = w.minimize();
                    }
                    CloseAction::Quit => {
                        if app_shell::quit_may_proceed(app, &shell) {
                            app.exit(0);
                        }
                    }
                }
            }
        })
        .setup(move |app| {
            let dir = app.path().app_config_dir()?;
            setup_config.tunnel.start(dir.clone());
            let notifier = notify::Notifier::new(&app.config().identifier);
            app.manage(Shell::load(dir, notifier));
            if let Some(menu) = app_shell::menu(app.handle())? {
                app.set_menu(menu)?;
            }
            #[cfg(not(target_os = "macos"))]
            {
                let has_tray = tray::install(app.handle())?;
                if let Some(shell) = app.try_state::<Shell>() {
                    shell.set_has_tray(has_tray);
                }
            }

            let start = if probe_mode { probe::PROBE_PATH } else { "/" };
            let url = app_url(start).parse()?;
            // Links the person shares name the public origin, never the app's
            // internal scheme (`apps/web/src/platform/index.ts`).
            let public_origin = serde_json::to_string(&setup_config.api_origin.origin().ascii_serialization())?;
            let mut builder = WebviewWindowBuilder::new(app, MAIN, WebviewUrl::CustomProtocol(url))
                .initialization_script(&format!(
                    "Object.defineProperty(globalThis, '__LAKKO_PUBLIC_ORIGIN__', {{ value: {public_origin}, writable: false, configurable: false }});"
                ));
            if cfg!(debug_assertions) {
                // Debug builds only: page errors and console errors reach the
                // host log, since the window has no devtools.
                builder = builder.initialization_script(probe::ERROR_REPORTER);
            }
            // The title is always "Lakko": task switchers, window lists and
            // screen-sharing pickers read it (`desktop.md` section 2).
            let window = builder
                .title("Lakko")
                .inner_size(1100.0, 760.0)
                .min_inner_size(820.0, 560.0)
                .visible(false)
                .build()?;
            if let Some(shell) = app.try_state::<Shell>() {
                app_shell::restore_geometry(&window, &shell);
            }
            if !start_hidden {
                window.show()?;
            }
            if probe_mode {
                probe::schedule_hide(app.handle().clone(), window.label().to_owned());
            }
            info!(hidden = start_hidden, "window opened");
            Ok(())
        })
        .build(tauri::generate_context!());

    let app = match built {
        Ok(a) => a,
        Err(e) => {
            error!(error = %e, "tauri build failed");
            std::process::exit(1);
        },
    };
    app.run(|app, event| match event {
        RunEvent::ExitRequested {
            code: None, api, ..
        } => {
            if let Some(shell) = app.try_state::<Shell>() {
                if let Some(w) = app.get_webview_window(MAIN) {
                    app_shell::remember_geometry(&w, &shell);
                }
                if !app_shell::quit_may_proceed(app, &shell) {
                    api.prevent_exit();
                }
            }
        },
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => app_shell::show_main(app),
        RunEvent::Exit => info!("exit"),
        _ => {},
    });
    warn!("event loop returned");
}
