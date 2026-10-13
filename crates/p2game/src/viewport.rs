//! Viewport queries script makes through `ConsoleCommand`: `GetCurrentRes`, `GetFullScreen`,
//! `GetCurrentColorDepth` and `get ini:Engine.Engine.ViewportManager <Key>`.
//!
//! `PlayerController.PostBeginPlay` calls `FOV(DesiredFOV)`, which runs `CalculateFOV` and
//! `ApplyAspectRatio`: `Res.X / Res.Y` from `GetResolution()`. With no answers that is 0/0 and the
//! player's `DefaultFOV`, `DesiredFOV` and `FOVAngle` all become NaN. This host answers from the
//! current viewport size (the window sets it; headless keeps the default) and is installed as a
//! level hook so it is in place before BeginPlay of every level.
use std::cell::Cell;
use std::rc::Rc;
use uscript::vm::{ObjId, Vm};

thread_local! {
    static SIZE: Cell<(u32, u32)> = const { Cell::new((1600, 900)) };
}

/// Sets the viewport size in pixels (the window calls this on resize).
pub fn set_size(w: u32, h: u32) {
    SIZE.with(|s| s.set((w.max(1), h.max(1))));
}

pub fn size() -> (u32, u32) {
    SIZE.with(|s| s.get())
}

struct ViewportHost;

/// Reply for a viewport console command, or None when it is not one.
pub fn answer(command: &str) -> Option<String> {
    let (w, h) = size();
    let c = command.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase();
    Some(match c.as_str() {
        "getcurrentres" => format!("{w}x{h}"),
        "getfullscreen" => "False".into(),
        "getcurrentcolordepth" => "32".into(),
        _ => {
            let k = c.strip_prefix("get ini:engine.engine.viewportmanager ")?;
            match k {
                "windowedviewportx" | "fullscreenviewportx" => w.to_string(),
                "windowedviewporty" | "fullscreenviewporty" => h.to_string(),
                "startupfullscreen" => "False".into(),
                _ => return None,
            }
        }
    })
}

impl uscript::host::Host for ViewportHost {
    fn console_command(&self, _vm: &mut Vm, _from: Option<ObjId>, command: &str) -> String {
        answer(command).unwrap_or_default()
    }
}

fn level_hook(host: &crate::host::GameHost, _vm: &mut Vm) {
    host.ext.borrow_mut().push(Rc::new(ViewportHost));
}

/// Registers the viewport host for every level. Call before the first level boots.
pub fn install() {
    crate::host::register_level_hook(level_hook);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_queries() {
        set_size(1600, 900);
        assert_eq!(answer("GetCurrentRes").as_deref(), Some("1600x900"));
        assert_eq!(answer("get ini:Engine.Engine.ViewportManager WindowedViewportY").as_deref(), Some("900"));
        assert_eq!(answer("get ini:Engine.Engine.ViewportManager StartupFullscreen").as_deref(), Some("False"));
        assert_eq!(answer("get ini:Engine.Engine.ViewportManager Foo"), None);
        assert_eq!(answer("SaveGame 3"), None);
    }
}
