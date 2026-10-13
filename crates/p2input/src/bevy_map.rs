//! Bevy keyboard/mouse/gamepad to UE2 `EInputKey` mapping (feature `bevy`).

use crate::keys::{Action, Key};
use bevy::input::gamepad::GamepadButton;
use bevy::input::keyboard::KeyCode;
use bevy::input::mouse::MouseButton;

/// UE2 key for a Bevy key code (physical key positions, US layout names like the original).
pub fn key_from_keycode(k: KeyCode) -> Option<Key> {
    use KeyCode as K;
    Some(match k {
        K::Backspace => Key::BACKSPACE,
        K::Tab => Key::TAB,
        K::Enter | K::NumpadEnter => Key::ENTER,
        K::ShiftLeft => Key::SHIFT,
        K::ShiftRight => Key::SHIFT,
        K::ControlLeft | K::ControlRight => Key::CTRL,
        K::AltLeft | K::AltRight => Key::ALT,
        K::Pause => Key::PAUSE,
        K::CapsLock => Key::CAPS_LOCK,
        K::Escape => Key::ESCAPE,
        K::Space => Key::SPACE,
        K::PageUp => Key::PAGE_UP,
        K::PageDown => Key::PAGE_DOWN,
        K::End => Key::END,
        K::Home => Key::HOME,
        K::ArrowLeft => Key::LEFT,
        K::ArrowUp => Key::UP,
        K::ArrowRight => Key::RIGHT,
        K::ArrowDown => Key::DOWN,
        K::PrintScreen => Key::PRINT_SCRN,
        K::Insert => Key::INSERT,
        K::Delete => Key::DELETE,
        K::Digit0 => Key(0x30),
        K::Digit1 => Key(0x31),
        K::Digit2 => Key(0x32),
        K::Digit3 => Key(0x33),
        K::Digit4 => Key(0x34),
        K::Digit5 => Key(0x35),
        K::Digit6 => Key(0x36),
        K::Digit7 => Key(0x37),
        K::Digit8 => Key(0x38),
        K::Digit9 => Key(0x39),
        K::KeyA => Key(b'A'),
        K::KeyB => Key(b'B'),
        K::KeyC => Key(b'C'),
        K::KeyD => Key(b'D'),
        K::KeyE => Key(b'E'),
        K::KeyF => Key(b'F'),
        K::KeyG => Key(b'G'),
        K::KeyH => Key(b'H'),
        K::KeyI => Key(b'I'),
        K::KeyJ => Key(b'J'),
        K::KeyK => Key(b'K'),
        K::KeyL => Key(b'L'),
        K::KeyM => Key(b'M'),
        K::KeyN => Key(b'N'),
        K::KeyO => Key(b'O'),
        K::KeyP => Key(b'P'),
        K::KeyQ => Key(b'Q'),
        K::KeyR => Key(b'R'),
        K::KeyS => Key(b'S'),
        K::KeyT => Key(b'T'),
        K::KeyU => Key(b'U'),
        K::KeyV => Key(b'V'),
        K::KeyW => Key(b'W'),
        K::KeyX => Key(b'X'),
        K::KeyY => Key(b'Y'),
        K::KeyZ => Key(b'Z'),
        K::Numpad0 => Key(0x60),
        K::Numpad1 => Key(0x61),
        K::Numpad2 => Key(0x62),
        K::Numpad3 => Key(0x63),
        K::Numpad4 => Key(0x64),
        K::Numpad5 => Key(0x65),
        K::Numpad6 => Key(0x66),
        K::Numpad7 => Key(0x67),
        K::Numpad8 => Key(0x68),
        K::Numpad9 => Key(0x69),
        K::NumpadMultiply => Key::GREY_STAR,
        K::NumpadAdd => Key::GREY_PLUS,
        K::NumpadSubtract => Key::GREY_MINUS,
        K::NumpadDecimal => Key::NUMPAD_PERIOD,
        K::NumpadDivide => Key::GREY_SLASH,
        K::F1 => Key(0x70),
        K::F2 => Key(0x71),
        K::F3 => Key(0x72),
        K::F4 => Key(0x73),
        K::F5 => Key(0x74),
        K::F6 => Key(0x75),
        K::F7 => Key(0x76),
        K::F8 => Key(0x77),
        K::F9 => Key(0x78),
        K::F10 => Key(0x79),
        K::F11 => Key(0x7A),
        K::F12 => Key(0x7B),
        K::NumLock => Key::NUM_LOCK,
        K::ScrollLock => Key::SCROLL_LOCK,
        K::Semicolon => Key::SEMICOLON,
        K::Equal => Key::EQUALS,
        K::Comma => Key::COMMA,
        K::Minus => Key::MINUS,
        K::Period => Key::PERIOD,
        K::Slash => Key::SLASH,
        K::Backquote => Key::TILDE,
        K::BracketLeft => Key::LEFT_BRACKET,
        K::Backslash => Key::BACKSLASH,
        K::BracketRight => Key::RIGHT_BRACKET,
        K::Quote => Key::SINGLE_QUOTE,
        _ => return None,
    })
}

pub fn key_from_mouse(b: MouseButton) -> Option<Key> {
    Some(match b {
        MouseButton::Left => Key::LEFT_MOUSE,
        MouseButton::Right => Key::RIGHT_MOUSE,
        MouseButton::Middle => Key::MIDDLE_MOUSE,
        MouseButton::Back => Key::MOUSE4,
        MouseButton::Forward => Key::MOUSE5,
        _ => return None,
    })
}

/// Gamepad buttons map to Joy1..Joy16 in DirectInput order (A, B, X, Y, LB, RB, Back, Start,
/// LS, RS, then the d-pad as Joy11..14 and the triggers as Joy15/16).
pub fn key_from_gamepad(b: GamepadButton) -> Option<Key> {
    use GamepadButton as G;
    let n = match b {
        G::South => 1,
        G::East => 2,
        G::West => 3,
        G::North => 4,
        G::LeftTrigger => 5,
        G::RightTrigger => 6,
        G::Select => 7,
        G::Start => 8,
        G::LeftThumb => 9,
        G::RightThumb => 10,
        G::DPadUp => 11,
        G::DPadDown => 12,
        G::DPadLeft => 13,
        G::DPadRight => 14,
        G::LeftTrigger2 => 15,
        G::RightTrigger2 => 16,
        _ => return None,
    };
    Key::joy(n)
}

/// One UE2 input event produced from Bevy input this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputEvent {
    pub key: Key,
    pub action: Action,
    /// Axis movement (mouse pixels, stick value) for `IST_Axis`, else 0.
    pub delta: f32,
}

/// Collects this frame's UE2 events from Bevy's button inputs and mouse motion: presses and
/// releases for keys, mouse buttons and gamepad buttons, `IST_Axis` for mouse X/Y (Y inverted:
/// UE2 mouse Y grows upward) and wheel steps as press+release of MouseWheelUp/Down.
pub fn collect_events(
    keys: &bevy::input::ButtonInput<KeyCode>,
    mouse: &bevy::input::ButtonInput<MouseButton>,
    motion: bevy::math::Vec2,
    wheel: f32,
    pads: &[&bevy::input::ButtonInput<GamepadButton>],
) -> Vec<InputEvent> {
    let mut out = Vec::new();
    let mut push = |key: Option<Key>, action: Action| {
        if let Some(key) = key {
            out.push(InputEvent { key, action, delta: 0.0 });
        }
    };
    for k in keys.get_just_pressed() {
        push(key_from_keycode(*k), Action::Press);
    }
    for k in keys.get_just_released() {
        push(key_from_keycode(*k), Action::Release);
    }
    for b in mouse.get_just_pressed() {
        push(key_from_mouse(*b), Action::Press);
    }
    for b in mouse.get_just_released() {
        push(key_from_mouse(*b), Action::Release);
    }
    for p in pads {
        for b in p.get_just_pressed() {
            push(key_from_gamepad(*b), Action::Press);
        }
        for b in p.get_just_released() {
            push(key_from_gamepad(*b), Action::Release);
        }
    }
    if motion.x != 0.0 {
        out.push(InputEvent { key: Key::MOUSE_X, action: Action::Axis, delta: motion.x });
    }
    if motion.y != 0.0 {
        out.push(InputEvent { key: Key::MOUSE_Y, action: Action::Axis, delta: -motion.y });
    }
    let wk = if wheel > 0.0 { Some(Key::MOUSE_WHEEL_UP) } else if wheel < 0.0 { Some(Key::MOUSE_WHEEL_DOWN) } else { None };
    if let Some(k) = wk {
        for _ in 0..(wheel.abs().round().max(1.0) as usize) {
            out.push(InputEvent { key: k, action: Action::Press, delta: 0.0 });
            out.push(InputEvent { key: k, action: Action::Release, delta: 0.0 });
        }
    }
    out
}

/// The character typed for `KeyType` events (Bevy text input), if printable.
pub fn typed_char(text: &str) -> Option<char> {
    text.chars().next().filter(|c| !c.is_control())
}
