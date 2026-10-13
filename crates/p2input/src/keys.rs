//! UE2 input codes: `Interactions.EInputKey` and `Interactions.EInputAction`.
//!
//! Key codes are the byte values of the script enum (which follow the Win32 virtual-key numbering
//! for keyboard keys). Key names are the enum names without the `IK_` prefix, which is how
//! User.ini binds them (`LeftMouse=Fire`, `Escape=ShowMenu`).

/// `EInputAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Action {
    None = 0,
    Press = 1,
    Hold = 2,
    Release = 3,
    Axis = 4,
}

impl Action {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Press,
            2 => Self::Hold,
            3 => Self::Release,
            4 => Self::Axis,
            _ => Self::None,
        }
    }
}

/// Names of every `EInputKey` value, indexed by code (without the `IK_` prefix).
pub const KEY_NAMES: [&str; 255] = [
    "None", "LeftMouse", "RightMouse", "Cancel", "MiddleMouse", "Unknown05", "Unknown06", "Unknown07", "Backspace", "Tab", "Unknown0A", "Unknown0B",
    "Unknown0C", "Enter", "Unknown0E", "Unknown0F", "Shift", "Ctrl", "Alt", "Pause", "CapsLock", "Unknown15", "Unknown16", "Unknown17", "Unknown18",
    "Unknown19", "Unknown1A", "Escape", "Unknown1C", "Unknown1D", "Unknown1E", "Unknown1F", "Space", "PageUp", "PageDown", "End", "Home", "Left", "Up",
    "Right", "Down", "Select", "Print", "Execute", "PrintScrn", "Insert", "Delete", "Help", "0", "1", "2", "3", "4", "5", "6", "7", "8", "9",
    "Unknown3A", "Unknown3B", "Unknown3C", "Unknown3D", "Unknown3E", "Unknown3F", "Unknown40", "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K",
    "L", "M", "N", "O", "P", "Q", "R", "S", "T", "U", "V", "W", "X", "Y", "Z", "Unknown5B", "Unknown5C", "Unknown5D", "Unknown5E", "Unknown5F",
    "NumPad0", "NumPad1", "NumPad2", "NumPad3", "NumPad4", "NumPad5", "NumPad6", "NumPad7", "NumPad8", "NumPad9", "GreyStar", "GreyPlus", "Separator",
    "GreyMinus", "NumPadPeriod", "GreySlash", "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "F13", "F14", "F15", "F16",
    "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24", "Unknown88", "Unknown89", "Unknown8A", "Unknown8B", "Unknown8C", "Unknown8D", "Unknown8E",
    "Unknown8F", "NumLock", "ScrollLock", "Unknown92", "Unknown93", "Unknown94", "Unknown95", "Unknown96", "Unknown97", "Unknown98", "Unknown99",
    "Unknown9A", "Unknown9B", "Unknown9C", "Unknown9D", "Unknown9E", "Unknown9F", "LShift", "RShift", "LControl", "RControl", "UnknownA4", "UnknownA5",
    "UnknownA6", "UnknownA7", "UnknownA8", "UnknownA9", "UnknownAA", "UnknownAB", "UnknownAC", "UnknownAD", "UnknownAE", "UnknownAF", "UnknownB0",
    "UnknownB1", "UnknownB2", "UnknownB3", "UnknownB4", "UnknownB5", "UnknownB6", "UnknownB7", "UnknownB8", "Unicode", "Semicolon", "Equals", "Comma",
    "Minus", "Period", "Slash", "Tilde", "Mouse4", "Mouse5", "Mouse6", "Mouse7", "Mouse8", "UnknownC6", "UnknownC7", "Joy1", "Joy2", "Joy3", "Joy4",
    "Joy5", "Joy6", "Joy7", "Joy8", "Joy9", "Joy10", "Joy11", "Joy12", "Joy13", "Joy14", "Joy15", "Joy16", "UnknownD8", "UnknownD9", "UnknownDA",
    "LeftBracket", "Backslash", "RightBracket", "SingleQuote", "UnknownDF", "UnknownE0", "UnknownE1", "UnknownE2", "UnknownE3", "MouseX", "MouseY",
    "MouseZ", "MouseW", "JoyU", "JoyV", "JoySlider1", "JoySlider2", "MouseWheelUp", "MouseWheelDown", "Unknown10E", "Unknown10F", "JoyX", "JoyY",
    "JoyZ", "JoyR", "UnknownF4", "UnknownF5", "Attn", "CrSel", "ExSel", "ErEof", "Play", "Zoom", "NoName", "PA1", "OEMClear",
];

/// An `EInputKey` code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key(pub u8);

macro_rules! keys {
    ($($name:ident = $v:expr),* $(,)?) => {
        impl Key { $( pub const $name: Key = Key($v); )* }
    };
}

keys! {
    NONE = 0x00, LEFT_MOUSE = 0x01, RIGHT_MOUSE = 0x02, MIDDLE_MOUSE = 0x04, BACKSPACE = 0x08, TAB = 0x09, ENTER = 0x0D,
    SHIFT = 0x10, CTRL = 0x11, ALT = 0x12, PAUSE = 0x13, CAPS_LOCK = 0x14, ESCAPE = 0x1B, SPACE = 0x20, PAGE_UP = 0x21,
    PAGE_DOWN = 0x22, END = 0x23, HOME = 0x24, LEFT = 0x25, UP = 0x26, RIGHT = 0x27, DOWN = 0x28, PRINT_SCRN = 0x2C,
    INSERT = 0x2D, DELETE = 0x2E, K0 = 0x30, A = 0x41, NUMPAD0 = 0x60, GREY_STAR = 0x6A, GREY_PLUS = 0x6B, GREY_MINUS = 0x6D,
    NUMPAD_PERIOD = 0x6E, GREY_SLASH = 0x6F, F1 = 0x70, NUM_LOCK = 0x90, SCROLL_LOCK = 0x91, LSHIFT = 0xA0, RSHIFT = 0xA1,
    LCONTROL = 0xA2, RCONTROL = 0xA3, UNICODE = 0xB9, SEMICOLON = 0xBA, EQUALS = 0xBB, COMMA = 0xBC, MINUS = 0xBD, PERIOD = 0xBE,
    SLASH = 0xBF, TILDE = 0xC0, MOUSE4 = 0xC1, MOUSE5 = 0xC2, JOY1 = 0xC8, LEFT_BRACKET = 0xDB, BACKSLASH = 0xDC,
    RIGHT_BRACKET = 0xDD, SINGLE_QUOTE = 0xDE, MOUSE_X = 0xE4, MOUSE_Y = 0xE5, MOUSE_Z = 0xE6, MOUSE_W = 0xE7, JOY_U = 0xE8,
    JOY_V = 0xE9, JOY_SLIDER1 = 0xEA, JOY_SLIDER2 = 0xEB, MOUSE_WHEEL_UP = 0xEC, MOUSE_WHEEL_DOWN = 0xED, JOY_X = 0xF0,
    JOY_Y = 0xF1, JOY_Z = 0xF2, JOY_R = 0xF3,
}

impl Key {
    /// Letter key A..Z.
    pub fn letter(c: char) -> Option<Key> {
        let c = c.to_ascii_uppercase();
        c.is_ascii_uppercase().then(|| Key(c as u8))
    }
    /// Digit key 0..9.
    pub fn digit(d: u8) -> Option<Key> {
        (d < 10).then(|| Key(0x30 + d))
    }
    /// Function key F1..F24.
    pub fn function(n: u8) -> Option<Key> {
        (1..=24).contains(&n).then(|| Key(0x70 + n - 1))
    }
    /// Joystick button 1..16.
    pub fn joy(n: u8) -> Option<Key> {
        (1..=16).contains(&n).then(|| Key(0xC8 + n - 1))
    }
    /// Name as used in User.ini (`IK_` prefix removed).
    pub fn name(self) -> &'static str {
        KEY_NAMES.get(self.0 as usize).copied().unwrap_or("")
    }
    /// Parses a key name (case-insensitive, optional `IK_` prefix) or a decimal number.
    pub fn from_name(s: &str) -> Option<Key> {
        let s = s.trim();
        let s = s.strip_prefix("IK_").or_else(|| s.strip_prefix("ik_")).unwrap_or(s);
        if let Some(i) = KEY_NAMES.iter().position(|n| n.eq_ignore_ascii_case(s)) {
            return Some(Key(i as u8));
        }
        // The script enum misspells 0x10F as UK_Unknown10F.
        if s.eq_ignore_ascii_case("UK_Unknown10F") {
            return Some(Key(0xEF));
        }
        None
    }
    /// True for analog axes (mouse/joystick movement): these produce `IST_Axis` events.
    pub fn is_axis(self) -> bool {
        matches!(self.0, 0xE4..=0xEB | 0xF0..=0xF3)
    }
    pub fn is_mouse_button(self) -> bool {
        matches!(self.0, 0x01 | 0x02 | 0x04 | 0xC1..=0xC5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        assert_eq!(KEY_NAMES.len(), 255);
        assert_eq!(Key::from_name("LeftMouse"), Some(Key::LEFT_MOUSE));
        assert_eq!(Key::from_name("IK_Escape"), Some(Key::ESCAPE));
        assert_eq!(Key::from_name("mousewheelup"), Some(Key::MOUSE_WHEEL_UP));
        assert_eq!(Key::from_name("F10"), Key::function(10));
        assert_eq!(Key::from_name("Tilde"), Some(Key::TILDE));
        assert_eq!(Key::from_name("JoyX"), Some(Key::JOY_X));
        assert_eq!(Key::from_name("OEMClear"), Some(Key(254)));
        assert_eq!(Key::A.name(), "A");
        assert_eq!(Key::letter('w'), Some(Key(0x57)));
        assert!(Key::MOUSE_X.is_axis() && !Key::A.is_axis());
    }
}
