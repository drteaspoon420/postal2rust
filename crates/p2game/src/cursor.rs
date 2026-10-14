//! OS cursor and app lifetime for the windowed game.
//!
//! - Cursor lock: while the game window has focus and the game owns the mouse (first-person look,
//!   [`UiInput::game_look`]), the OS cursor is hidden and locked to the window so mouse look never
//!   leaves it (UE2 captures the mouse the same way). Menus, the console, the dev menu/editor, and
//!   losing focus (Alt+Tab) release it. `P2GAME_NO_GRAB=1` disables the lock.
//! - Quit: the `exit` / `quit` console command (main menu Exit, Quit to desktop) sets
//!   [`p2game::host::quit_requested`]; this closes the app (window, audio, process).

use crate::ui::UiInput;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

pub(crate) struct CursorPlugin;

impl Plugin for CursorPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Last, (quit_on_request, cursor_lock));
    }
}

fn quit_on_request(mut exit: MessageWriter<AppExit>, mut sent: Local<bool>, mut windows: Query<&mut CursorOptions, With<PrimaryWindow>>) {
    if !*sent && p2game::host::quit_requested() {
        *sent = true;
        println!("quit: exit requested by script, closing");
        if let Ok(mut cur) = windows.single_mut() {
            cur.grab_mode = CursorGrabMode::None;
            cur.visible = true;
        }
        release_os_clip();
        exit.write(AppExit::Success);
    }
}

/// Called after the app loop ends (any exit path: menu, window close, F-key test exits).
pub(crate) fn release_cursor() {
    release_os_clip();
}

/// Frees the OS cursor clip directly (winit's confine is a `ClipCursor` rectangle that can
/// outlive the window when it closes or loses focus mid-frame).
fn release_os_clip() {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn ClipCursor(rect: *const std::ffi::c_void) -> i32;
        }
        unsafe {
            ClipCursor(std::ptr::null());
        }
    }
}

/// Scripted/automated runs drive input themselves; never grab the user's real mouse there
/// (`P2GAME_GRAB=1` forces the lock on for tests of the lock itself).
fn automated() -> bool {
    if std::env::var_os("P2GAME_GRAB").is_some() {
        return false;
    }
    ["P2GAME_NO_GRAB", "P2GAME_EXEC", "P2GAME_PILOT", "P2GAME_REAL", "P2GAME_HOLD", "P2GAME_SHOTS", "P2GAME_EXIT", "P2GAME_DEMO_GOTO", "P2GAME_DEMO_LOOK"].iter().any(|v| std::env::var_os(v).is_some())
}

fn cursor_lock(ui: Option<Res<UiInput>>, dev: Option<Res<crate::devmenu::DevState>>, mut windows: Query<(&Window, &mut CursorOptions), With<PrimaryWindow>>, mut auto: Local<Option<bool>>) {
    let auto = *auto.get_or_insert_with(automated);
    let Ok((win, mut cur)) = windows.single_mut() else { return };
    let dev_open = dev.is_some_and(|d| d.open || d.editor);
    let look = ui.is_some_and(|u| u.game_look && !u.captured);
    let want = !auto && win.focused && look && !dev_open;
    // Locked (relative motion, stays in place) where supported, else confined to the window.
    let mode = if want {
        if cfg!(windows) { CursorGrabMode::Confined } else { CursorGrabMode::Locked }
    } else {
        CursorGrabMode::None
    };
    if cur.grab_mode != mode {
        if std::env::var_os("P2GAME_GRAB_TRACE").is_some() {
            println!("cursor: grab {:?} -> {mode:?} (focused {} look {look} dev {dev_open})", cur.grab_mode, win.focused);
        }
        cur.grab_mode = mode;
        if mode == CursorGrabMode::None {
            release_os_clip();
        }
    }
    if cur.visible == want {
        cur.visible = !want;
    }
}
