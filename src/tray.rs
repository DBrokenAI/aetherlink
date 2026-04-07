//! `aetherlink --tray` — minimal system tray supervisor.
//!
//! Polls the status file written by the MCP server and reflects the project
//! state in the tray icon and tooltip. Click "Quit" in the menu to exit.
//!
//! Intentionally tiny: no popups, no settings panel, no embedded webview. The
//! goal is a glanceable health indicator that lives next to the clock.

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder};

use crate::status::{self, State, Status};

pub fn run() -> Result<()> {
    eprintln!("AetherLink tray starting. Right-click the tray icon to quit.");

    let event_loop = EventLoopBuilder::new().build();

    // Build the menu (just Quit for now — keep it tiny).
    let menu = Menu::new();
    let quit_item = MenuItem::new("Quit AetherLink tray", true, None);
    menu.append(&quit_item)?;
    let quit_id = quit_item.id().clone();

    // Build the tray icon with an "unknown" state to start.
    let tray = TrayIconBuilder::new()
        .with_tooltip("AetherLink — waiting for first scan…")
        .with_icon(make_icon(State::Unknown))
        .with_menu(Box::new(menu))
        .build()?;

    // Worker thread polls the status file and pushes Status updates over a
    // channel. The tray itself is not Send on Windows, so we keep it on the
    // main thread and only ferry plain data across threads.
    let (tx, rx) = mpsc::channel::<Status>();
    thread::spawn(move || {
        let mut last_state: Option<State> = None;
        loop {
            thread::sleep(Duration::from_secs(2));
            match status::read() {
                Ok(Some(status)) => {
                    if last_state != Some(status.state) {
                        last_state = Some(status.state);
                        let _ = tx.send(status);
                    }
                }
                Ok(None) => {} // no scan yet — leave the icon "unknown"
                Err(e) => tracing::warn!("status read failed: {e}"),
            }
        }
    });

    let menu_channel = MenuEvent::receiver();

    event_loop.run(move |event, _, control_flow| {
        // Wake up periodically so we can drain channels even when nothing
        // is happening on the OS event side.
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(500));

        // Drain queued status updates and update the tray.
        while let Ok(status) = rx.try_recv() {
            let _ = tray.set_icon(Some(make_icon(status.state)));
            let _ = tray.set_tooltip(Some(format_tooltip(&status)));
        }

        // Drain queued menu events.
        while let Ok(ev) = menu_channel.try_recv() {
            if ev.id == quit_id {
                *control_flow = ControlFlow::Exit;
                return;
            }
        }

        // Avoid touching `event` directly — we just need wakeups.
        let _ = event;
    });
}

/// Build a 32×32 solid-color icon. We don't need shading or a logo — the goal
/// is just an unmistakable color in the tray.
fn make_icon(state: State) -> Icon {
    let (r, g, b) = match state {
        State::Legal => (40, 200, 80),    // green
        State::Illegal => (220, 50, 50),  // red
        State::Unknown => (150, 150, 150), // gray
    };
    let size: u32 = 32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            // 1px darker border so the icon edge is visible against any tray bg.
            let on_border = x == 0 || y == 0 || x == size - 1 || y == size - 1;
            if on_border {
                rgba.extend_from_slice(&[r / 2, g / 2, b / 2, 255]);
            } else {
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
    }
    Icon::from_rgba(rgba, size, size).expect("static RGBA buffer is valid")
}

fn format_tooltip(status: &Status) -> String {
    match status.state {
        State::Legal => format!("AetherLink — LEGAL\n{}", status.project_path),
        State::Illegal => format!(
            "AetherLink — {} VIOLATION{}\n{}",
            status.violation_count,
            if status.violation_count == 1 { "" } else { "S" },
            status.project_path
        ),
        State::Unknown => "AetherLink — no scans yet".to_string(),
    }
}
