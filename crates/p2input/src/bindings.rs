//! `[Engine.Input]` bindings from User.ini: key bindings and command aliases.
//!
//! ```text
//! Aliases[2]=(Command="Axis aBaseY  Speed=+1200.0",alias="MoveForward")
//! W=MoveForward
//! MouseX=Count bXAxis | Axis aMouseX Speed=2.0
//! I=set d3drenderdevice reducemouselag false | onrelease set d3drenderdevice reducemouselag true
//! ```

use crate::keys::Key;

pub const MAX_ALIASES: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alias {
    pub alias: String,
    pub command: String,
}

#[derive(Debug, Clone)]
pub struct Bindings {
    /// Binding string per key code (empty = unbound).
    pub keys: Vec<String>,
    /// The 40 alias slots (unused ones have alias "None").
    pub aliases: Vec<Alias>,
}

impl Default for Bindings {
    fn default() -> Self {
        Self { keys: vec![String::new(); 256], aliases: vec![Alias { alias: "None".into(), command: String::new() }; MAX_ALIASES] }
    }
}

/// Parses `(Command="...",alias="...")` (field names case-insensitive, quotes optional).
pub fn parse_alias(s: &str) -> Option<Alias> {
    let s = s.trim();
    let inner = s.strip_prefix('(')?.strip_suffix(')')?;
    let (mut command, mut alias) = (None, None);
    let mut rest = inner;
    while !rest.trim().is_empty() {
        let r = rest.trim_start().trim_start_matches(',').trim_start();
        let eq = r.find('=')?;
        let key = r[..eq].trim();
        let after = &r[eq + 1..];
        let (val, next) = if let Some(q) = after.strip_prefix('"') {
            let end = q.find('"')?;
            (&q[..end], &q[end + 1..])
        } else {
            let end = after.find(',').unwrap_or(after.len());
            (&after[..end], &after[end..])
        };
        if key.eq_ignore_ascii_case("Command") {
            command = Some(val.to_string());
        } else if key.eq_ignore_ascii_case("Alias") {
            alias = Some(val.to_string());
        }
        rest = next;
    }
    Some(Alias { alias: alias.unwrap_or_else(|| "None".into()), command: command.unwrap_or_default() })
}

impl Bindings {
    /// Reads the `[Engine.Input]` section of an ini text (User.ini). Later lines override earlier.
    pub fn from_ini(text: &str) -> Self {
        Self::from_section_lines(section_lines(text, "Engine.Input"))
    }

    /// Builds bindings from `Key=Value` lines of the input section.
    pub fn from_section_lines<'a>(lines: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut b = Bindings::default();
        let mut next_alias = 0usize;
        for (k, v) in lines {
            let k = k.trim();
            if k.len() >= 7 && k[..7].eq_ignore_ascii_case("Aliases") {
                let idx = k[7..].trim().strip_prefix('[').and_then(|t| t.strip_suffix(']')).and_then(|t| t.trim().parse::<usize>().ok());
                let idx = idx.unwrap_or(next_alias);
                next_alias = idx + 1;
                if let (Some(a), true) = (parse_alias(v), idx < MAX_ALIASES) {
                    b.aliases[idx] = a;
                }
                continue;
            }
            if let Some(key) = Key::from_name(k) {
                b.keys[key.0 as usize] = v.trim().to_string();
            }
        }
        b
    }

    pub fn binding(&self, key: Key) -> &str {
        self.keys.get(key.0 as usize).map(|s| s.as_str()).unwrap_or("")
    }

    pub fn set_binding(&mut self, key: Key, cmd: &str) {
        if let Some(k) = self.keys.get_mut(key.0 as usize) {
            *k = cmd.trim().to_string();
        }
    }

    pub fn alias(&self, name: &str) -> Option<&str> {
        if name.is_empty() || name.eq_ignore_ascii_case("None") {
            return None;
        }
        self.aliases.iter().find(|a| a.alias.eq_ignore_ascii_case(name)).map(|a| a.command.as_str())
    }

    /// Pieces of a key's binding split at `|`, trimmed (leading single space dropped, upper-cased
    /// comparisons are done by the callers).
    pub fn pieces(&self, key: Key) -> Vec<&str> {
        let b = self.binding(key);
        if b.is_empty() {
            return vec![];
        }
        b.split('|').map(|p| p.trim()).collect()
    }

    /// `n`-th key (0-based) whose binding contains `command` as one of its `|` pieces
    /// (`BINDING2KEYVAL` / `GetCommandKey`).
    pub fn command_key(&self, command: &str, n: usize) -> Option<Key> {
        let mut found = 0;
        for k in 0..255u8 {
            let key = Key(k);
            if self.pieces(key).iter().any(|p| p.eq_ignore_ascii_case(command.trim())) {
                if found == n {
                    return Some(key);
                }
                found += 1;
            }
        }
        None
    }

    /// Writes the section back as ini lines (for SaveConfig / `set input`).
    pub fn to_ini_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (i, a) in self.aliases.iter().enumerate() {
            out.push(format!("Aliases[{i}]=(Command=\"{}\",Alias=\"{}\")", a.command, a.alias));
        }
        for k in 0..255u8 {
            let key = Key(k);
            if key == Key::NONE {
                continue;
            }
            out.push(format!("{}={}", key.name(), self.binding(key)));
        }
        out
    }
}

/// `(key, value)` lines of one ini section (case-insensitive name), comments skipped.
pub fn section_lines<'a>(text: &'a str, section: &str) -> Vec<(&'a str, &'a str)> {
    let mut out = Vec::new();
    let mut inside = false;
    for raw in text.lines() {
        let l = raw.trim_end_matches('\r');
        let t = l.trim();
        if t.starts_with('[') && t.ends_with(']') {
            inside = t[1..t.len() - 1].trim().eq_ignore_ascii_case(section);
            continue;
        }
        if !inside || t.is_empty() || t.starts_with(';') {
            continue;
        }
        if let Some((k, v)) = l.split_once('=') {
            out.push((k.trim(), v));
        }
    }
    out
}

/// Reads ini text that may be UTF-16 (with BOM) or Latin-1/UTF-8.
pub fn decode_ini(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let u: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&u);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.trim_start_matches('\u{feff}').to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INI: &str = "[Engine.Input]\r\nAliases[0]=(Command=\"Button bFire | Fire\",alias=\"Fire\")\r\nAliases[2]=(Command=\"Axis aBaseY  Speed=+1200.0\",alias=\"MoveForward\")\r\nAliases[30]=(Command=\"ShowScoreboard|OnRelease HideScoreboard\",Alias=\"ScoreToggle\")\r\nLeftMouse=Fire\r\nW=MoveForward\r\nMouseX=Count bXAxis | Axis aMouseX Speed=2.0 \r\nEscape=ShowMenu\r\n[Other]\r\nA=Nope\r\n";

    #[test]
    fn parses_aliases_and_keys() {
        let b = Bindings::from_ini(INI);
        assert_eq!(b.alias("fire"), Some("Button bFire | Fire"));
        assert_eq!(b.alias("MoveForward"), Some("Axis aBaseY  Speed=+1200.0"));
        assert_eq!(b.alias("ScoreToggle"), Some("ShowScoreboard|OnRelease HideScoreboard"));
        assert_eq!(b.binding(Key::LEFT_MOUSE), "Fire");
        assert_eq!(b.binding(Key::letter('w').unwrap()), "MoveForward");
        assert_eq!(b.binding(Key::MOUSE_X), "Count bXAxis | Axis aMouseX Speed=2.0");
        assert_eq!(b.binding(Key::A), "", "keys from other sections are ignored");
        assert_eq!(b.pieces(Key::MOUSE_X), vec!["Count bXAxis", "Axis aMouseX Speed=2.0"]);
        assert_eq!(b.command_key("ShowMenu", 0), Some(Key::ESCAPE));
        assert_eq!(b.command_key("ShowMenu", 1), None);
    }
}
