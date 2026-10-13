//! `P2GAME_REAL`: injects synthetic OS input as Bevy input messages, upstream of everything the
//! game reads (`ButtonInput<KeyCode>`, mouse buttons, accumulated motion and wheel). Unlike
//! `P2GAME_HOLD`, which hands UE2 events straight to the front end, this exercises the same path
//! as a player's keyboard and mouse: Bevy's input systems, `p2input::bevy_map`, typed text,
//! bindings and the UWindow / console / HUD interactions.
//!
//! Spec: `;`-separated `frame:action`, frames counted in rendered frames since start:
//! - `40:key Escape` press+release (`down`/`up` for one side only), Bevy `KeyCode` names
//!   (`KeyW`, `Digit2`, `Backquote`, `Enter`, `Escape`, `Space`, `ShiftLeft`, ...)
//! - `40:type summon BasePeople.Bystanders` types text, one character per frame, with
//!   KeyboardInput `text` set (what the console's KeyType reads)
//! - `40:click left` / `40:mdown left` / `40:mup left` (left/right/middle)
//! - `40:move 120,-30` relative mouse motion (pixels, Bevy convention: +y is down)
//! - `40:cursor 800,450` absolute cursor position in the window
//! - `40:wheel -1` wheel steps
//!
//! When active, the real OS input still flows too; tests should keep the window unfocused.

use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::mouse::{MouseButtonInput, MouseMotion, MouseScrollUnit, MouseWheel};
use bevy::input::touch::TouchPhase;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub(crate) struct RealInputPlugin;

impl Plugin for RealInputPlugin {
    fn build(&self, app: &mut App) {
        if let Ok(spec) = std::env::var("P2GAME_REAL") {
            app.insert_resource(RealScript { steps: parse(&spec), frame: 0, pending: Vec::new() })
                .add_systems(PreUpdate, inject.before(bevy::input::InputSystems));
        }
    }
}

#[derive(Debug, Clone)]
enum Step {
    Key(KeyCode, Option<bool>),
    Type(String),
    Mouse(MouseButton, Option<bool>),
    Move(f32, f32),
    Cursor(f32, f32),
    Wheel(f32),
}

#[derive(Resource)]
struct RealScript {
    steps: Vec<(u32, Step)>,
    frame: u32,
    /// Deferred releases / typed characters: (frame, step).
    pending: Vec<(u32, Step)>,
}

fn keycode(name: &str) -> Option<KeyCode> {
    let n = name.trim();
    // Short forms: a single letter or digit.
    if n.len() == 1 {
        let c = n.chars().next()?.to_ascii_uppercase();
        if c.is_ascii_alphabetic() {
            return keycode(&format!("Key{c}"));
        }
        if c.is_ascii_digit() {
            return keycode(&format!("Digit{c}"));
        }
    }
    Some(match n {
        "Escape" | "Esc" => KeyCode::Escape,
        "Enter" | "Return" => KeyCode::Enter,
        "Space" => KeyCode::Space,
        "Backquote" | "Tilde" | "`" => KeyCode::Backquote,
        "Backspace" => KeyCode::Backspace,
        "Tab" => KeyCode::Tab,
        "ShiftLeft" | "Shift" => KeyCode::ShiftLeft,
        "ControlLeft" | "Ctrl" => KeyCode::ControlLeft,
        "ArrowUp" | "Up" => KeyCode::ArrowUp,
        "ArrowDown" | "Down" => KeyCode::ArrowDown,
        "ArrowLeft" | "Left" => KeyCode::ArrowLeft,
        "ArrowRight" | "Right" => KeyCode::ArrowRight,
        "F1" => KeyCode::F1,
        "F5" => KeyCode::F5,
        "F9" => KeyCode::F9,
        "F3" => KeyCode::F3,
        "F10" => KeyCode::F10,
        "F11" => KeyCode::F11,
        "Delete" | "Del" => KeyCode::Delete,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "Period" | "." => KeyCode::Period,
        "Minus" | "-" => KeyCode::Minus,
        _ => {
            let k = n.strip_prefix("Key").or_else(|| n.strip_prefix("Digit"))?;
            let c = k.chars().next()?;
            match (n.starts_with("Key"), c) {
                (true, 'A'..='Z') => [
                    KeyCode::KeyA, KeyCode::KeyB, KeyCode::KeyC, KeyCode::KeyD, KeyCode::KeyE, KeyCode::KeyF, KeyCode::KeyG, KeyCode::KeyH, KeyCode::KeyI,
                    KeyCode::KeyJ, KeyCode::KeyK, KeyCode::KeyL, KeyCode::KeyM, KeyCode::KeyN, KeyCode::KeyO, KeyCode::KeyP, KeyCode::KeyQ, KeyCode::KeyR,
                    KeyCode::KeyS, KeyCode::KeyT, KeyCode::KeyU, KeyCode::KeyV, KeyCode::KeyW, KeyCode::KeyX, KeyCode::KeyY, KeyCode::KeyZ,
                ][(c as u8 - b'A') as usize],
                (false, '0'..='9') => [
                    KeyCode::Digit0, KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3, KeyCode::Digit4, KeyCode::Digit5, KeyCode::Digit6, KeyCode::Digit7,
                    KeyCode::Digit8, KeyCode::Digit9,
                ][(c as u8 - b'0') as usize],
                _ => return None,
            }
        }
    })
}

/// The key that types `c` (US layout), with shift.
fn key_for_char(c: char) -> Option<(KeyCode, bool)> {
    let (base, shift) = match c {
        'A'..='Z' => (c.to_ascii_lowercase(), true),
        '_' => ('-', true),
        _ => (c, false),
    };
    let k = match base {
        'a'..='z' | '0'..='9' => keycode(&base.to_string())?,
        ' ' => KeyCode::Space,
        '.' => KeyCode::Period,
        '-' => KeyCode::Minus,
        '`' => KeyCode::Backquote,
        _ => return None,
    };
    Some((k, shift))
}

fn button(name: &str) -> MouseButton {
    match name.trim() {
        "right" => MouseButton::Right,
        "middle" => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

fn parse(spec: &str) -> Vec<(u32, Step)> {
    let mut out = Vec::new();
    for item in spec.split(';') {
        let Some((n, rest)) = item.split_once(':') else { continue };
        let Ok(f) = n.trim().parse::<u32>() else { continue };
        let (verb, arg) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
        let pair = |a: &str| {
            let (x, y) = a.split_once(',').unwrap_or((a, "0"));
            (x.trim().parse().unwrap_or(0.0), y.trim().parse().unwrap_or(0.0))
        };
        let step = match verb {
            "key" => keycode(arg).map(|k| Step::Key(k, None)),
            "down" => keycode(arg).map(|k| Step::Key(k, Some(true))),
            "up" => keycode(arg).map(|k| Step::Key(k, Some(false))),
            "type" => Some(Step::Type(arg.to_string())),
            "click" => Some(Step::Mouse(button(arg), None)),
            "mdown" => Some(Step::Mouse(button(arg), Some(true))),
            "mup" => Some(Step::Mouse(button(arg), Some(false))),
            "move" => Some({
                let (x, y) = pair(arg);
                Step::Move(x, y)
            }),
            "cursor" => Some({
                let (x, y) = pair(arg);
                Step::Cursor(x, y)
            }),
            "wheel" => Some(Step::Wheel(arg.trim().parse().unwrap_or(-1.0))),
            _ => None,
        };
        match step {
            Some(s) => out.push((f, s)),
            None => eprintln!("P2GAME_REAL: bad step {item:?}"),
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn inject(
    mut st: ResMut<RealScript>,
    mut windows: Query<(Entity, &mut Window), With<PrimaryWindow>>,
    mut os: (
        ResMut<Messages<KeyboardInput>>,
        ResMut<Messages<MouseButtonInput>>,
        ResMut<Messages<MouseMotion>>,
        ResMut<Messages<MouseWheel>>,
    ),
) {
    // Scripted runs own the input: drop whatever the OS delivered this frame (a focused window
    // would otherwise move the view or click), then write the script's messages.
    os.0.drain().for_each(drop);
    os.1.drain().for_each(drop);
    os.2.drain().for_each(drop);
    os.3.drain().for_each(drop);
    let (keys, buttons, motion, wheel) = (&mut *os.0, &mut *os.1, &mut *os.2, &mut *os.3);
    st.frame += 1;
    let f = st.frame;
    let Ok((window, mut win)) = windows.single_mut() else { return };
    let mut due: Vec<Step> = st.steps.iter().filter(|(n, _)| *n == f).map(|(_, s)| s.clone()).collect();
    let (now, later): (Vec<_>, Vec<_>) = std::mem::take(&mut st.pending).into_iter().partition(|(n, _)| *n <= f);
    st.pending = later;
    due.extend(now.into_iter().map(|(_, s)| s));
    let key_ev = |k: KeyCode, down: bool, text: Option<char>| KeyboardInput {
        key_code: k,
        logical_key: match text {
            Some(c) => Key::Character(c.to_string().into()),
            None => Key::Unidentified(bevy::input::keyboard::NativeKey::Unidentified),
        },
        state: if down { ButtonState::Pressed } else { ButtonState::Released },
        text: if down { text.map(|c| c.to_string().into()) } else { None },
        repeat: false,
        window,
    };
    for s in due {
        println!("REAL@{f} {s:?}");
        match s {
            Step::Key(k, None) => {
                keys.write(key_ev(k, true, None));
                st.pending.push((f + 2, Step::Key(k, Some(false))));
            }
            Step::Key(k, Some(d)) => {
                keys.write(key_ev(k, d, None));
            }
            Step::Type(text) => {
                // One character per frame: press (with text) now, release next frame.
                let mut chars = text.chars();
                if let Some(c) = chars.next() {
                    if let Some((k, shift)) = key_for_char(c) {
                        if shift {
                            keys.write(key_ev(KeyCode::ShiftLeft, true, None));
                        }
                        keys.write(key_ev(k, true, Some(c)));
                        st.pending.push((f + 1, Step::Key(k, Some(false))));
                        if shift {
                            st.pending.push((f + 1, Step::Key(KeyCode::ShiftLeft, Some(false))));
                        }
                    }
                    let rest: String = chars.collect();
                    if !rest.is_empty() {
                        st.pending.push((f + 2, Step::Type(rest)));
                    }
                }
            }
            Step::Mouse(b, None) => {
                buttons.write(MouseButtonInput { button: b, state: ButtonState::Pressed, window });
                st.pending.push((f + 2, Step::Mouse(b, Some(false))));
            }
            Step::Mouse(b, Some(d)) => {
                buttons.write(MouseButtonInput { button: b, state: if d { ButtonState::Pressed } else { ButtonState::Released }, window });
            }
            Step::Move(x, y) => {
                motion.write(MouseMotion { delta: Vec2::new(x, y) });
            }
            Step::Cursor(x, y) => {
                win.set_cursor_position(Some(Vec2::new(x, y)));
            }
            Step::Wheel(y) => {
                wheel.write(MouseWheel { unit: MouseScrollUnit::Line, x: 0.0, y, window, phase: TouchPhase::Moved });
            }
        }
    }
}
