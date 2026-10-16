//! Remembers where the user left the game window: position and size are written to
//! `<user dir>\window.txt` when the user moves or resizes it (interactive runs only), and every
//! launch, including scripted test runs, opens the windowed game there. Test runs never write the
//! file, so they can't move the window the user arranged.

use bevy::prelude::*;
use bevy::window::{PrimaryWindow, WindowMode, WindowMoved, WindowPosition, WindowResized};
use std::path::PathBuf;

fn path() -> Option<PathBuf> {
    uscript::config::default_user_dir().map(|d| d.join("window.txt"))
}

/// Saved (x, y, width, height) in physical pixels.
pub(crate) fn saved() -> Option<(i32, i32, u32, u32)> {
    let s = std::fs::read_to_string(path()?).ok()?;
    let n: Vec<i64> = s.split_whitespace().filter_map(|t| t.parse().ok()).collect();
    let (x, y, w, h) = (*n.first()?, *n.get(1)?, *n.get(2)?, *n.get(3)?);
    (w >= 320 && h >= 200 && w <= 16384 && h <= 16384 && x.abs() < 100_000 && y.abs() < 100_000).then_some((x as i32, y as i32, w as u32, h as u32))
}

/// Applies the saved rect to a windowed primary window description.
pub(crate) fn apply_saved(w: &mut Window) {
    if !matches!(w.mode, WindowMode::Windowed) || std::env::var_os("P2GAME_RES").is_some() {
        // An explicit P2GAME_RES still wins for size; keep the position though.
        if let (Some((x, y, _, _)), WindowMode::Windowed) = (saved(), w.mode) {
            w.position = WindowPosition::At(IVec2::new(x, y));
        }
        return;
    }
    if let Some((x, y, width, height)) = saved() {
        w.position = WindowPosition::At(IVec2::new(x, y));
        w.resolution.set_physical_resolution(width, height);
    }
}

fn scripted() -> bool {
    let a: Vec<String> = std::env::args().collect();
    a.iter().any(|x| x == "--smoke" || x == "--progress" || x == "--tour" || x == "--profile-secs" || x == "--screenshot")
        || ["P2GAME_EXEC", "P2GAME_PILOT", "P2GAME_REAL", "P2GAME_SHOTS", "P2GAME_EXIT", "P2GAME_EXIT_SECS", "P2GAME_DEMO_GOTO", "P2GAME_MUTE"]
            .iter()
            .any(|v| std::env::var(v).is_ok_and(|s| !s.trim().is_empty()))
}

pub(crate) struct WindowMemoryPlugin;

impl Plugin for WindowMemoryPlugin {
    fn build(&self, app: &mut App) {
        if !scripted() {
            app.add_systems(Last, remember);
        }
    }
}

/// Writes the window rect a moment after the user stops moving/resizing it.
fn remember(
    mut moved: MessageReader<WindowMoved>,
    mut resized: MessageReader<WindowResized>,
    windows: Query<&Window, With<PrimaryWindow>>,
    time: Res<Time>,
    mut pending: Local<Option<f32>>,
    mut last: Local<Option<(i32, i32, u32, u32)>>,
    mut pos: Local<Option<IVec2>>,
) {
    let mut changed = resized.read().count() > 0;
    for m in moved.read() {
        *pos = Some(m.position);
        changed = true;
    }
    let now = time.elapsed_secs();
    if changed {
        *pending = Some(now + 0.5);
    }
    let Some(due) = *pending else { return };
    if now < due {
        return;
    }
    *pending = None;
    let Ok(w) = windows.single() else { return };
    if !matches!(w.mode, WindowMode::Windowed) {
        return;
    }
    let p = match (*pos, w.position) {
        (Some(p), _) => p,
        (None, WindowPosition::At(p)) => p,
        _ => return,
    };
    let rect = (p.x, p.y, w.resolution.physical_width(), w.resolution.physical_height());
    if *last == Some(rect) || rect.2 < 320 || rect.3 < 200 {
        return;
    }
    *last = Some(rect);
    if let Some(f) = path() {
        let _ = std::fs::write(f, format!("{} {} {} {}\n", rect.0, rect.1, rect.2, rect.3));
    }
}
