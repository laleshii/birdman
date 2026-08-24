mod assets;
mod compose;
mod config;
mod cursor;
mod onboarding;
mod palette;
mod root;
mod selectable;
mod state;
mod text_input;
mod theme;
mod webview;

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{prelude::*, px, size, App, Application, Bounds, WindowBounds, WindowOptions};

use config::{Config, ConfiguredAccount};
use onboarding::Onboarding;
use root::Root;
use state::AppState;

fn main() {
    // Before any other thread exists: it edits the environment.
    #[cfg(target_os = "linux")]
    let use_x11 = prefer_xwayland_if_available();

    let config = config::load();

    Application::new()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            #[cfg(target_os = "linux")]
            if use_x11 {
                init_gtk_for_webview(cx);
            }

            let accounts = match config {
                Config::Accounts(accounts) => accounts,
                Config::Unconfigured { path, error } => {
                    open_onboarding_window(cx, path, error);
                    cx.activate(true);
                    return;
                }
            };

            launch_main_app(cx, accounts);
            cx.activate(true);
        });
}

/// gpui picks its Linux backend once from `WAYLAND_DISPLAY` vs `DISPLAY`
/// (`gpui::guess_compositor`) and cannot swap afterwards, while `wry`'s
/// child-webview embedding rejects a native Wayland handle outright. So hide
/// `WAYLAND_DISPLAY` before gpui reads it, steering onto XWayland where it
/// exists. A Wayland-only session is left alone.
///
/// Returns whether gpui will end up on X11.
#[cfg(target_os = "linux")]
fn prefer_xwayland_if_available() -> bool {
    let has_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
    let has_x11 = std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty());
    if has_wayland && has_x11 {
        // SAFETY: the first thing `main` does, before any other thread exists.
        unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
    }
    has_x11
}

/// `wry`'s X11 embedding calls `gdk::Display::default()` before it even looks
/// at the window handle, and GDK *panics* if GTK was never initialized. gpui
/// drives X11 directly and never touches GTK, so this must run before the first
/// webview attach -- and GTK's main loop must keep turning afterwards, from the
/// thread GTK was initialized on, or the WebKitGTK widget never paints.
///
/// `webview::mark_gtk_ready` gates on this succeeding, so a failure degrades to
/// the plaintext fallback rather than the panic.
#[cfg(target_os = "linux")]
fn init_gtk_for_webview(cx: &mut App) {
    if let Err(err) = gtk::init() {
        log::error!("GTK init failed, HTML bodies will show plaintext only: {err}");
        return;
    }
    webview::mark_gtk_ready();
    cx.spawn(async move |cx| {
        loop {
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
            // WebKitGTK renders cross-process, so a load is several IPC hops
            // and each only appears once the previous response lands. At 16ms
            // that added 16ms per hop, compounding into multi-second stalls.
            cx.background_executor()
                .timer(std::time::Duration::from_millis(4))
                .await;
        }
    })
    .detach();
}

gpui::actions!(birdman, [Quit, CloseWindow, Find, Palette]);

pub(crate) fn launch_main_app(cx: &mut App, _accounts: Vec<ConfiguredAccount>) {
    let data_dir = birdman_config::data_dir();
    std::fs::create_dir_all(&data_dir).expect("failed to create data directory");
    birdman_config::logging::init(&data_dir);

    let service = match birdman_client::Client::connect() {
        Ok(client) => Arc::new(client),
        Err(err) => {
            log::error!("could not reach birdmand: {err}");
            eprintln!("birdman-desktop: could not reach birdmand: {err}");
            cx.quit();
            return;
        }
    };

    // From the daemon, not the config file: it has already reconciled config
    // against the store, so its ids are the ones messages are keyed by. The
    // signing name is the exception -- config, not mailbox state, with no
    // column in the store -- so it is matched back on by address.
    let signing_names: Vec<(String, Option<String>)> = match birdman_config::load() {
        birdman_config::Config::Accounts(accounts) => {
            accounts.into_iter().map(|a| (a.email, a.name)).collect()
        }
        birdman_config::Config::Unconfigured { .. } => Vec::new(),
    };
    let runtimes: Vec<state::AccountRuntime> = service
        .accounts()
        .unwrap_or_default()
        .into_iter()
        .map(|account| state::AccountRuntime {
            id: account.id,
            display_name: account.display_name,
            name: signing_names
                .iter()
                .find(|(email, _)| email.eq_ignore_ascii_case(&account.email))
                .and_then(|(_, name)| name.clone()),
            email: account.email,
        })
        .collect();
    for account in &runtimes {
        log::info!("account {} ({})", account.display_name, account.email);
    }

    let app_state = cx.new(|cx| AppState::new(cx, service.clone(), runtimes));
    app_state.update(cx, |state, cx| {
        state.watch_appearance(cx);
        state.refresh_sync_status(cx);
        state.refresh_folders(cx)
    });

    // Every shortcut below is an app-menu action rather than a `handle_key`
    // branch: the reading pane's webview is a native child view, and while
    // focus is inside it gpui never sees the key event at all. The platform
    // menu fires regardless of which view has focus.
    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
    // Closes whichever window is frontmost.
    cx.on_action(|_: &CloseWindow, cx: &mut App| {
        let Some(window) = cx.active_window() else {
            return;
        };
        // Deferred: gpui `take()`s the window for the duration of an update, so
        // a nested `window.update` fails with "window not found".
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        });
    });
    cx.on_action({
        let app_state = app_state.clone();
        move |_: &Find, cx: &mut App| {
            let Some(window) = cx.active_window() else {
                return;
            };
            let app_state = app_state.clone();
            cx.defer(move |cx| {
                let _ = window.update(cx, move |_, window, cx| {
                    app_state.update(cx, |state, cx| state.toggle_search(window, cx));
                });
            });
        }
    });
    cx.on_action({
        let app_state = app_state.clone();
        move |_: &Palette, cx: &mut App| {
            let Some(window) = cx.active_window() else {
                return;
            };
            let app_state = app_state.clone();
            cx.defer(move |cx| {
                let _ = window.update(cx, move |_, _, cx| {
                    app_state.update(cx, |state, cx| {
                        let open = !state.palette_open;
                        state.set_palette(open, cx);
                    });
                });
            });
        }
    });
    // `KeyBinding` takes a literal chord, so the modifier must be chosen:
    // "cmd-k" never fires on a Linux keyboard. `root::handle_key` needs no such
    // care, matching `Modifiers::secondary()` instead.
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    cx.bind_keys([
        gpui::KeyBinding::new(&format!("{modifier}-k"), Palette, None),
        gpui::KeyBinding::new(&format!("{modifier}-f"), Find, None),
        gpui::KeyBinding::new(&format!("{modifier}-w"), CloseWindow, None),
        gpui::KeyBinding::new(&format!("{modifier}-q"), Quit, None),
    ]);
    cx.set_menus(vec![gpui::Menu {
        name: "Birdman".into(),
        items: vec![
            gpui::MenuItem::action("Command Palette", Palette),
            gpui::MenuItem::action("Find", Find),
            gpui::MenuItem::action("Close Window", CloseWindow),
            gpui::MenuItem::action("Quit", Quit),
        ],
    }]);
    // A windowless gpui app lingers as a headless process otherwise.
    cx.on_window_closed(|cx| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();

    let font_family = ui_font_family(cx);
    let bounds = Bounds::centered(None, size(px(1000.0), px(700.0)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Birdman".into()),
                // The app draws its own title strip (`root::titlebar`), so both
                // are painted and the window shows "Birdman" twice otherwise.
                appears_transparent: true,
                traffic_light_position: Some(gpui::point(px(12.0), px(10.0))),
            }),
            ..Default::default()
        },
        {
            let app_state = app_state.clone();
            move |window, cx| {
                let root = cx.new(|cx| Root {
                    state: app_state.clone(),
                    focus_handle: cx.focus_handle(),
                    webview: None,
                    webview_positioned_for: None,
                    font_family,
                });
                root.update(cx, |root, cx| {
                    root.focus_handle.focus(window);
                    root.state.update(cx, |state, _| {
                        state.root_focus_handle = Some(root.focus_handle.clone());
                    });
                });
                root
            }
        },
    )
    .expect("failed to open window");

    // A loop, not a single `while let`: a subscription belongs to one daemon
    // process and ends when that process does, which would otherwise leave the
    // window silently event-dead -- still drawing, never updating. Each
    // resubscribe is followed by a full refresh, since events are deltas.
    //
    // Weak, so the loop has something to end on: `update` fails once the window
    // is gone, which is the only signal that resubscribing is unwanted.
    let app_state = app_state.downgrade();
    cx.spawn(async move |cx| loop {
        let events = match service.subscribe() {
            Ok(events) => events,
            Err(err) => {
                log::error!("could not subscribe to birdmand: {err}");
                return;
            }
        };
        pump_events(&events, &app_state, cx).await;
        log::warn!("event stream ended; resubscribing");
        if app_state
            .update(cx, |state, cx| {
                state.refresh_folders(cx);
                state.refresh_messages(cx);
                state.refresh_sync_status(cx);
            })
            .is_err()
        {
            return;
        }
    })
    .detach();
}

async fn pump_events(
    events: &async_channel::Receiver<birdman_proto::Event>,
    app_state: &gpui::WeakEntity<crate::state::AppState>,
    cx: &mut gpui::AsyncApp,
) {
    {
        while let Ok(event) = events.recv().await {
            let app_state = app_state.clone();
            if app_state
                .update(cx, |state, cx| match event {
                    birdman_proto::Event::FoldersChanged { .. } => state.refresh_folders(cx),
                    birdman_proto::Event::SyncProgress { folder, .. } => {
                        state.status = Some(match folder {
                            Some(name) => format!("Syncing {name}..."),
                            None => "Syncing...".to_string(),
                        });
                        cx.notify();
                    }
                    birdman_proto::Event::MessagesChanged { folder } => {
                        if state.selected_folder == Some(folder)
                            && !state.absorbed_own_change(folder)
                        {
                            state.refresh_messages(cx);
                        }
                    }
                    birdman_proto::Event::SyncIdle { .. } => {
                        // The event says *one* account finished, so writing
                        // "Synced" on it claimed the whole mailbox was done the
                        // moment the quickest account was. `refresh_sync_status`
                        // takes the worst state across accounts instead.
                        state.refresh_sync_status(cx);
                    }
                    birdman_proto::Event::SyncFailed { account, message } => {
                        // Named: a bare "sync error" in a log covering two accounts
                        // is unattributable.
                        let who = state
                            .account(account)
                            .map(|a| a.display_name.clone())
                            .unwrap_or_else(|| format!("account {}", account.0));
                        log::error!("{who}: sync error: {message}");
                        state.status = Some(credential_hint(&who, &message));
                        cx.notify();
                    }
                    birdman_proto::Event::OutboxChanged { .. } => {
                        // Sending on the desktop resolves when the message is
                        // queued, so there is nothing here to repaint.
                    }
                })
                .is_err()
            {
                return;
            }
        }
    }
}

/// Setup lives in the CLI, so the status line has to name the command --
/// otherwise a missing password looks like a broken app.
fn credential_hint(account: &str, message: &str) -> String {
    if message.contains("no credential found") || message.contains("OAuth2 refresh token") {
        return format!("{account}: no password saved -- run: birdman login <account>");
    }
    // Gmail answers a burst of logins with the same code it uses for a wrong
    // password, so this must not be reported as a bad credential.
    if message.contains("AUTHENTICATIONFAILED") {
        return format!(
            "{account}: server rejected the login -- check: birdman check-auth <account>"
        );
    }
    format!("{account}: {}", crate::state::short_error(message))
}

fn open_onboarding_window(cx: &mut App, path: PathBuf, error: Option<String>) {
    let bounds = Bounds::centered(None, size(px(560.0), px(320.0)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Birdman".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        move |_, cx| {
            cx.new(|_| Onboarding {
                config_path: path,
                error,
            })
        },
    )
    .expect("failed to open onboarding window");
}

/// Asks the text system what is installed rather than hardcoding a name.
/// gpui's cosmic-text backend matches a family by *literal string* -- there is
/// no generic-alias resolution, so a name matching nothing falls through
/// silently, with one tell: a single-face family makes `font_weight(BOLD)` a
/// no-op. Each candidate must register both a Regular and a Bold face.
fn ui_font_family(cx: &App) -> gpui::SharedString {
    const CANDIDATES: [&str; 6] = [
        ".SF NS",
        "SF Pro Text",
        "Inter",
        "Liberation Sans",
        "DejaVu Sans",
        "Helvetica Neue",
    ];
    let available = cx.text_system().all_font_names();
    CANDIDATES
        .iter()
        .find(|name| available.iter().any(|f| f == *name))
        .map(|name| gpui::SharedString::from(*name))
        .unwrap_or_else(|| gpui::SharedString::from("Helvetica"))
}
