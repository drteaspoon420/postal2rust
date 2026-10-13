//! Input processing: the engine side of `UInput` (key state, binding execution, `Button`/`Toggle`/
//! `Axis`/`Count`/`Pulse` commands, alias expansion, `OnRelease`) and the input-related console
//! queries script uses (`KEYNAME`, `KEYBINDING`, `BINDING2KEYSTR`, `BINDING2KEYVAL`, `ISKEYDOWN`,
//! `ISKEYBIND`, `SET INPUT`).
//!
//! Semantics (observed from the shipped engine):
//! - A key event first goes through press/release filtering: a press on an already-down key and a
//!   release on a key that is not down are dropped. Held keys get an `IST_Hold` event every frame.
//! - The key's binding is split at `|` into commands. Each command is run with the current action;
//!   on `IST_Press` every command runs, on `IST_Release` only commands prefixed `OnRelease` run
//!   (prefix removed). Commands whose first word is an alias are replaced by the alias text, which
//!   may itself contain `|`.
//! - `Button bX`: sets the input byte to 1 on press, clears it on release. `Toggle bX` flips bit 7 on
//!   press. `Count bX` increments it. `Pulse bX` sets it on press (cleared each frame by the owner).
//! - `Axis aX Speed=S [Invert=-1] [SpeedBase=B DeadZone=D]`: on `IST_Hold` adds `Delta * S * Invert`;
//!   on `IST_Axis` (mouse/joystick) adds `Delta * 0.01 * S * Invert`. With `SpeedBase` set the
//!   joystick dead-zone formula is used. Axes accumulate during a frame; at the end of the frame
//!   (`finish_frame`) each input float is scaled by `20 / DeltaTime`, matching UE2 `ReadInput`.
//! - Anything else is an exec command passed to the target (`Fire`, `ShowMenu`, `set ...`).

use crate::bindings::Bindings;
use crate::keys::{Action, Key};

/// Where input lands: the PlayerController's `input` variables and its exec functions.
pub trait InputTarget {
    /// Pointer-free access to an `input byte` variable (`bFire`, `bXAxis`, ...). None = unknown name.
    fn input_byte(&mut self, name: &str) -> Option<u8>;
    fn set_input_byte(&mut self, name: &str, v: u8);
    /// `input float` variables (`aBaseY`, `aMouseX`, ...).
    fn input_float(&mut self, name: &str) -> Option<f32>;
    fn set_input_float(&mut self, name: &str, v: f32);
    /// Executes a console/exec command (e.g. `Fire`, `ShowMenu`, `Jump`). Returns true if handled.
    fn exec(&mut self, command: &str) -> bool;
}

#[derive(Debug, Clone)]
pub struct Input {
    pub bindings: Bindings,
    down: [bool; 256],
    action: Action,
    delta: f32,
    /// Names of every `input float` touched this frame (for the end-of-frame scale).
    touched_axes: Vec<String>,
    /// Exclusive menu mode (`MENUEXCLUSIVEMODE 1`): only the menu-navigation buttons pass.
    pub menu_exclusive: bool,
    /// `(key name, binding)` changed by `set input` since the host last saved them
    /// (UE2 `UInput::SetBinding` writes the key to User.ini immediately).
    pub changed_bindings: Vec<(String, String)>,
}

impl Input {
    pub fn new(bindings: Bindings) -> Self {
        Self { bindings, down: [false; 256], action: Action::None, delta: 0.0, touched_axes: Vec::new(), menu_exclusive: false, changed_bindings: Vec::new() }
    }

    pub fn is_down(&self, key: Key) -> bool {
        self.down[key.0 as usize]
    }

    /// Keys currently held.
    pub fn held_keys(&self) -> Vec<Key> {
        (0..255u8).map(Key).filter(|k| self.down[k.0 as usize]).collect()
    }

    /// Press/release filtering (`PreProcess`): returns false for a repeated press or a release of a
    /// key that is not down. Call before handing the event to interactions.
    pub fn pre_process(&mut self, key: Key, action: Action) -> bool {
        let d = &mut self.down[key.0 as usize];
        match action {
            Action::Press => {
                if *d {
                    return false;
                }
                *d = true;
            }
            Action::Release => {
                if !*d {
                    return false;
                }
                *d = false;
            }
            _ => {}
        }
        true
    }

    /// Releases every held key (focus loss, `ResetInput`), running release bindings.
    pub fn reset(&mut self, target: &mut dyn InputTarget) {
        for k in self.held_keys() {
            self.down[k.0 as usize] = false;
            self.process(target, k, Action::Release, 0.0);
        }
    }

    /// Runs the key's binding (`Process`). Returns true if the key had a binding.
    pub fn process(&mut self, target: &mut dyn InputTarget, key: Key, action: Action, delta: f32) -> bool {
        let binding = self.bindings.binding(key).to_string();
        if binding.is_empty() {
            return false;
        }
        self.action = action;
        self.delta = delta;
        self.exec_input_commands(target, &binding, 0);
        self.action = Action::None;
        self.delta = 0.0;
        true
    }

    /// Per-frame: `IST_Hold` for every held key, then axis scaling (`ReadInput`). `dt` in seconds.
    pub fn finish_frame(&mut self, target: &mut dyn InputTarget, dt: f32) {
        for k in self.held_keys() {
            if !k.is_axis() {
                self.process(target, k, Action::Hold, dt);
            }
        }
        let scale = if dt > 0.0 { 20.0 / dt } else { 0.0 };
        for name in std::mem::take(&mut self.touched_axes) {
            if let Some(v) = target.input_float(&name) {
                target.set_input_float(&name, v * scale);
            }
        }
    }

    /// Zeroes accumulated axes before a frame's events (the PlayerController does this in script
    /// via `PlayerInput`, but hosts without it can call this).
    pub fn clear_axes(&mut self, target: &mut dyn InputTarget, names: &[&str]) {
        for n in names {
            if target.input_float(n).is_some() {
                target.set_input_float(n, 0.0);
            }
        }
    }

    fn exec_input_commands(&mut self, target: &mut dyn InputTarget, line: &str, depth: u32) {
        if depth > 8 {
            return;
        }
        for piece in line.split('|') {
            let mut cmd = piece.trim();
            if cmd.is_empty() {
                continue;
            }
            let on_release = word_is(cmd, "OnRelease");
            if on_release {
                cmd = cmd[9..].trim();
            }
            // OnRelease commands run only on release; others run on press, hold and axis (the
            // input commands themselves decide what each action means).
            match (self.action, on_release) {
                (Action::Release, true) => {
                    let saved = self.action;
                    self.action = Action::Press;
                    self.exec_one(target, cmd, depth);
                    self.action = saved;
                }
                (_, true) => {}
                (Action::Release, false) => {
                    // Only input commands react to release (Button clears its byte).
                    if is_input_command(cmd) || (depth == 0 && self.alias_of(cmd).is_some()) {
                        self.exec_one(target, cmd, depth);
                    }
                }
                _ => self.exec_one(target, cmd, depth),
            }
        }
    }

    fn alias_of(&self, cmd: &str) -> Option<String> {
        let first = cmd.split_whitespace().next()?;
        self.bindings.alias(first).map(|a| {
            let rest = cmd[first.len()..].trim();
            if rest.is_empty() { a.to_string() } else { format!("{a} {rest}") }
        })
    }

    fn exec_one(&mut self, target: &mut dyn InputTarget, cmd: &str, depth: u32) {
        // Aliases expand once: inside an alias body names are not looked up again (so
        // `Fire = "Button bFire | Fire"` runs the exec function Fire), as in UE2.
        if depth == 0 {
            if let Some(expanded) = self.alias_of(cmd) {
                self.exec_input_commands(target, &expanded, depth + 1);
                return;
            }
        }
        if !self.exec_input_command(target, cmd) && matches!(self.action, Action::Press) {
            target.exec(cmd);
        }
    }

    /// `Button`/`Toggle`/`Axis`/`Count`/`Pulse`. Returns true if `cmd` was one of them.
    fn exec_input_command(&mut self, target: &mut dyn InputTarget, cmd: &str) -> bool {
        let mut it = cmd.split_whitespace();
        let Some(verb) = it.next() else { return false };
        let var = it.next().unwrap_or("");
        let v = verb.to_ascii_lowercase();
        match v.as_str() {
            "button" | "pulse" => {
                if let Some(b) = target.input_byte(var) {
                    match self.action {
                        Action::Press => target.set_input_byte(var, 1),
                        Action::Release if b != 0 && v == "button" => target.set_input_byte(var, 0),
                        _ => {}
                    }
                }
                true
            }
            "toggle" => {
                if let (Some(b), Action::Press) = (target.input_byte(var), self.action) {
                    target.set_input_byte(var, b ^ 0x80);
                }
                true
            }
            "count" => {
                if let Some(b) = target.input_byte(var) {
                    target.set_input_byte(var, b.wrapping_add(1));
                }
                true
            }
            "axis" => {
                let rest: Vec<&str> = it.collect();
                let p = |k: &str| rest.iter().find_map(|t| t.split_once('=').filter(|(a, _)| a.eq_ignore_ascii_case(k)).and_then(|(_, b)| b.parse::<f32>().ok()));
                let speed = p("Speed").unwrap_or(1.0);
                let speed_base = p("SpeedBase").unwrap_or(0.0);
                let invert = p("Invert").unwrap_or(1.0);
                let dead = p("DeadZone").unwrap_or(0.0);
                let delta = p("Delta").unwrap_or(self.delta);
                let Some(cur) = target.input_float(var) else { return true };
                let add = if speed_base <= 0.0 {
                    match self.action {
                        Action::Axis => delta * 0.01 * speed * invert,
                        Action::Hold => delta * speed * invert,
                        _ => 0.0,
                    }
                } else if speed.abs() > dead {
                    let norm = if speed > 0.0 { (speed - dead) / (1.0 - dead) } else { -((-speed - dead) / (1.0 - dead)) };
                    let d = if delta != 0.0 { delta } else { 1.0 / 60.0 };
                    speed_base * d * 60.0 * norm * invert
                } else {
                    0.0
                };
                if add != 0.0 {
                    target.set_input_float(var, cur + add);
                    if !self.touched_axes.iter().any(|n| n.eq_ignore_ascii_case(var)) {
                        self.touched_axes.push(var.to_string());
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// Input-related console queries. Returns `Some(reply)` if `cmd` was one of them.
    pub fn console_query(&mut self, cmd: &str) -> Option<String> {
        let t = cmd.trim();
        let (verb, rest) = t.split_once(char::is_whitespace).map(|(a, b)| (a, b.trim())).unwrap_or((t, ""));
        let toks: Vec<&str> = rest.split_whitespace().collect();
        let unq = |s: &str| s.trim_matches('"').to_string();
        Some(match verb.to_ascii_uppercase().as_str() {
            "KEYNAME" => Key(rest.trim().parse::<i32>().unwrap_or(0).clamp(0, 254) as u8).name().to_string(),
            "KEYNO" => Key::from_name(rest).map(|k| k.0.to_string()).unwrap_or_default(),
            "KEYBINDING" => Key::from_name(rest).map(|k| self.bindings.binding(k).to_string()).unwrap_or_default(),
            "BINDING2KEYSTR" | "BINDING2KEYVAL" => {
                // BINDING2KEYSTR "Command" n
                let (command, n) = split_quoted(rest);
                let n = n.trim().parse::<usize>().unwrap_or(0);
                match self.bindings.command_key(&command, n) {
                    Some(k) if verb.eq_ignore_ascii_case("BINDING2KEYSTR") => k.name().to_string(),
                    Some(k) => k.0.to_string(),
                    None if verb.eq_ignore_ascii_case("BINDING2KEYSTR") => String::new(),
                    None => "0".into(),
                }
            }
            "ISKEYDOWN" => (if self.is_down(Key(rest.parse::<i32>().unwrap_or(0).clamp(0, 254) as u8)) { "1" } else { "0" }).into(),
            "ISKEYBIND" => {
                // ISKEYBIND <key number or name> <Command>
                let (Some(k), Some(c)) = (toks.first(), toks.get(1)) else { return Some("0".into()) };
                let key = k.parse::<i32>().ok().filter(|v| *v != 0).map(|v| Key(v.clamp(0, 254) as u8)).or_else(|| Key::from_name(k));
                let hit = key.is_some_and(|k| self.bindings.pieces(k).iter().any(|p| p.eq_ignore_ascii_case(&unq(c))));
                (if hit { "1" } else { "0" }).into()
            }
            "MENUEXCLUSIVEMODE" => {
                self.menu_exclusive = rest.trim().parse::<i32>().unwrap_or(0) != 0;
                "1".into()
            }
            "SET" if toks.first().is_some_and(|s| s.eq_ignore_ascii_case("input")) => {
                // set input <Key> <command...>
                let Some(k) = toks.get(1).and_then(|k| Key::from_name(k)) else { return Some(String::new()) };
                let pos = rest.find(toks[1]).map(|p| p + toks[1].len()).unwrap_or(rest.len());
                self.bindings.set_binding(k, rest[pos..].trim());
                self.changed_bindings.push((k.name().to_string(), self.bindings.binding(k).to_string()));
                String::new()
            }
            _ => return None,
        })
    }
}

fn word_is(s: &str, w: &str) -> bool {
    s.len() >= w.len() && s[..w.len()].eq_ignore_ascii_case(w) && s[w.len()..].chars().next().is_none_or(|c| c.is_whitespace())
}

fn is_input_command(cmd: &str) -> bool {
    ["Button", "Toggle", "Axis", "Count", "Pulse"].iter().any(|w| word_is(cmd, w))
}

/// `"Some Command" 1` -> ("Some Command", "1"); unquoted first token otherwise.
fn split_quoted(s: &str) -> (String, &str) {
    let s = s.trim();
    if let Some(q) = s.strip_prefix('"') {
        if let Some(end) = q.find('"') {
            return (q[..end].to_string(), &q[end + 1..]);
        }
    }
    match s.split_once(char::is_whitespace) {
        Some((a, b)) => (a.to_string(), b),
        None => (s.to_string(), ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Pc {
        bytes: HashMap<String, u8>,
        floats: HashMap<String, f32>,
        execs: Vec<String>,
    }

    impl InputTarget for Pc {
        fn input_byte(&mut self, n: &str) -> Option<u8> {
            Some(*self.bytes.entry(n.to_ascii_lowercase()).or_default())
        }
        fn set_input_byte(&mut self, n: &str, v: u8) {
            self.bytes.insert(n.to_ascii_lowercase(), v);
        }
        fn input_float(&mut self, n: &str) -> Option<f32> {
            Some(*self.floats.entry(n.to_ascii_lowercase()).or_default())
        }
        fn set_input_float(&mut self, n: &str, v: f32) {
            self.floats.insert(n.to_ascii_lowercase(), v);
        }
        fn exec(&mut self, c: &str) -> bool {
            self.execs.push(c.to_string());
            true
        }
    }

    fn input() -> Input {
        Input::new(Bindings::from_ini(
            "[Engine.Input]\nAliases[0]=(Command=\"Button bFire | Fire\",alias=\"Fire\")\nAliases[1]=(Command=\"Axis aBaseY  Speed=+1200.0\",alias=\"MoveForward\")\nAliases[2]=(Command=\"ShowScoreboard|OnRelease HideScoreboard\",Alias=\"ScoreToggle\")\nLeftMouse=Fire\nW=MoveForward\nTab=ScoreToggle\nMouseX=Count bXAxis | Axis aMouseX Speed=2.0\nL=Toggle bLook\n",
        ))
    }

    #[test]
    fn button_and_exec_on_press_release() {
        let (mut i, mut pc) = (input(), Pc::default());
        assert!(i.pre_process(Key::LEFT_MOUSE, Action::Press));
        i.process(&mut pc, Key::LEFT_MOUSE, Action::Press, 0.0);
        assert_eq!(pc.bytes["bfire"], 1);
        assert_eq!(pc.execs, vec!["Fire"]);
        assert!(!i.pre_process(Key::LEFT_MOUSE, Action::Press), "repeat press filtered");
        assert!(i.pre_process(Key::LEFT_MOUSE, Action::Release));
        i.process(&mut pc, Key::LEFT_MOUSE, Action::Release, 0.0);
        assert_eq!(pc.bytes["bfire"], 0);
        assert_eq!(pc.execs.len(), 1, "exec commands do not run on release");
    }

    #[test]
    fn onrelease_alias_chain() {
        let (mut i, mut pc) = (input(), Pc::default());
        i.process(&mut pc, Key::TAB, Action::Press, 0.0);
        i.process(&mut pc, Key::TAB, Action::Release, 0.0);
        assert_eq!(pc.execs, vec!["ShowScoreboard", "HideScoreboard"]);
    }

    #[test]
    fn axis_hold_and_frame_scale() {
        let (mut i, mut pc) = (input(), Pc::default());
        let w = Key::letter('w').unwrap();
        i.pre_process(w, Action::Press);
        i.process(&mut pc, w, Action::Press, 0.0);
        i.finish_frame(&mut pc, 0.05);
        // Hold adds dt*Speed = 0.05*1200 = 60, then the frame scale 20/dt = 400 -> 24000.
        assert!((pc.floats["abasey"] - 24000.0).abs() < 1e-2, "{}", pc.floats["abasey"]);
    }

    #[test]
    fn mouse_axis_and_count() {
        let (mut i, mut pc) = (input(), Pc::default());
        i.process(&mut pc, Key::MOUSE_X, Action::Axis, 10.0);
        assert_eq!(pc.bytes["bxaxis"], 1);
        assert!((pc.floats["amousex"] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn toggle_flips_high_bit() {
        let (mut i, mut pc) = (input(), Pc::default());
        let l = Key::letter('l').unwrap();
        i.process(&mut pc, l, Action::Press, 0.0);
        assert_eq!(pc.bytes["blook"], 0x80);
        i.process(&mut pc, l, Action::Release, 0.0);
        i.process(&mut pc, l, Action::Press, 0.0);
        assert_eq!(pc.bytes["blook"], 0);
    }

    #[test]
    fn console_queries() {
        let mut i = input();
        assert_eq!(i.console_query("KEYNAME 27").unwrap(), "Escape");
        assert_eq!(i.console_query("BINDING2KEYSTR \"Fire\" 0").unwrap(), "LeftMouse");
        assert_eq!(i.console_query("BINDING2KEYVAL \"Fire\" 0").unwrap(), "1");
        assert_eq!(i.console_query("ISKEYBIND 1 Fire").unwrap(), "1");
        assert_eq!(i.console_query("ISKEYBIND 2 Fire").unwrap(), "0");
        assert_eq!(i.console_query("ISKEYDOWN 1").unwrap(), "0");
        i.console_query("set input F5 QuickSave").unwrap();
        assert_eq!(i.bindings.binding(Key::function(5).unwrap()), "QuickSave");
        assert!(i.console_query("Fire").is_none());
    }
}
