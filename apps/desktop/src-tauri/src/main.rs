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
mod call_window;
mod device_key;
mod key_store;
mod link;
mod notify;
mod probe;
mod proxy;
mod save_file;
#[cfg(not(target_os = "macos"))]
mod tray;
mod tunnel;

use std::sync::Arc;

use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tracing::{error, info, warn};

use app_shell::{CloseAction, Shell, MAIN};

/// The URI scheme the window's page is served from.
pub const APP_SCHEME: &str = "lakkoapp";

/// Where the page lives, as each OS's webview spells a custom scheme.
pub fn app_url(path: &str) -> String {
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
    // macOS asks for the camera and the microphone on behalf of the process
    // responsible for the app. Started through Launch Services the app is its
    // own; started as a child of a terminal or another app, that program is,
    // and the camera is then refused or offered under its name.
    #[cfg(target_os = "macos")]
    if std::os::unix::process::parent_id() != 1 {
        warn!(
            "started as a child process, not through Launch Services: macOS attributes camera and \
             microphone requests to the parent program (`just dev-desktop` starts the app with `open`)"
        );
    }

    let handler_config = Arc::clone(&config);
    let setup_config = Arc::clone(&config);
    let builder = tauri::Builder::default();
    // A second start hands its link to the running app and exits. macOS never
    // starts a second copy: it sends the running one an event instead.
    #[cfg(not(target_os = "macos"))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _| {
        link::from_args(app, &args);
    }));
    let built = builder
        .manage(link::Held::new(config.api_origin.origin().ascii_serialization()))
        .plugin(tauri_plugin_dialog::init())
        .enable_macos_default_menu(false)
        .register_asynchronous_uri_scheme_protocol(APP_SCHEME, move |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            let label = ctx.webview_label().to_owned();
            let config = Arc::clone(&handler_config);
            tauri::async_runtime::spawn(async move {
                let response = proxy::handle(&app, &config, &label, request).await;
                responder.respond(response);
            });
        })
        .on_menu_event(|app, event| app_shell::on_menu(app, event.id().as_ref()))
        .on_window_event(|window, event| {
            if window.label() == call_window::LABEL {
                if let WindowEvent::Moved(at) = event {
                    if let Some(state) = window.app_handle().try_state::<call_window::CallWindow>() {
                        state.remember_position(*at);
                    }
                }
                return;
            }
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
            app.manage(call_window::CallWindow::default());
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

            let start = if probe_mode { probe::start_path() } else { "/".to_owned() };
            let url = app_url(&start).parse()?;
            // Links the person shares name the public origin, never the app's
            // internal scheme (`apps/web/src/platform/index.ts`).
            let public_origin = serde_json::to_string(&setup_config.api_origin.origin().ascii_serialization())?;
            let mut builder = WebviewWindowBuilder::new(app, MAIN, WebviewUrl::CustomProtocol(url))
                .initialization_script(format!(
                    "Object.defineProperty(globalThis, '__LAKKO_PUBLIC_ORIGIN__', {{ value: {public_origin}, writable: false, configurable: false }});"
                ))
                // A new page has no collapsed call: whatever the old one
                // relayed to the call window goes with it.
                .on_page_load(|w, payload| {
                    if payload.event() == tauri::webview::PageLoadEvent::Started {
                        call_window::reset(w.app_handle());
                    }
                });
            if !probe_mode || probe::keep_throttling_off() {
                // The page must keep running while this window is minimised
                // or hidden: the collapsed call's own window draws frames it
                // sends (`call_window.rs`), and it holds the delivery socket.
                // On macOS 14 and later this sets WebKit's inactive
                // scheduling policy to none. Measured with the probe on
                // macOS 27.2, window minimised, no call: with WebKit's default
                // the page stopped altogether about four minutes after it was
                // hidden (no timer, no request) until it was shown again;
                // with none it ran for the seven minutes measured. Timers are
                // throttled either way (a page timer fires once a second, a
                // worker's about eight times), so the call window's clock
                // runs in a worker (`desktop-call-window-ticker.ts`). The
                // setting applies to the whole run, not only during a call;
                // its battery cost is not measured. Older macOS, Windows and
                // Linux ignore it (wry applies it on WKWebView only).
                builder = builder.background_throttling(tauri::utils::config::BackgroundThrottlingPolicy::Disabled);
            }
            if cfg!(debug_assertions) {
                // Debug builds only: page errors and console errors reach the
                // host log, since the window has no devtools.
                builder = builder.initialization_script(probe::ERROR_REPORTER);
            }
            // The title is always "Lakko": task switchers, window lists and
            // screen-sharing pickers read it (`desktop.md` section 2).
            let window = builder
                .title("Lakko")
                // The page's own drop targets take a dragged file; the host's
                // handler would swallow the event before them.
                .disable_drag_drop_handler()
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
            #[cfg(not(target_os = "macos"))]
            link::from_args(app.handle(), &std::env::args().collect::<Vec<_>>());
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
        #[cfg(target_os = "macos")]
        RunEvent::Opened { urls } => {
            for url in urls {
                link::arrived(app, url.as_str());
            }
        },
        RunEvent::Exit => info!("exit"),
        _ => {},
    });
    warn!("event loop returned");
}
