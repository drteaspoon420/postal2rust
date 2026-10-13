//! Clean-room parser for Unreal Engine 2 style `.ini` files.
//!
//! Supported syntax (as seen in the shipped `System\*.ini` files):
//! - `[Section]` headers, matched case-insensitively. A section that appears twice is merged.
//! - `Key=Value` lines. Repeating a key keeps every value in order (UE2 uses this for lists such as
//!   `Paths=` and `ServerPackages=`).
//! - Indexed keys `Key[3]=Value` are stored under `Key` with their index (fixed arrays such as
//!   `Aliases[0]`).
//! - `+Key=Value` appends a value unless it is already present, `-Key=Value` removes a matching
//!   value, `.Key=Value` always appends, `!Key=` clears every value of the key.
//! - Lines starting with `;` or `#` are comments. Whitespace around keys is trimmed and trailing
//!   whitespace (including the tabs some shipped lines carry) is trimmed from values.
//! - Struct values `(A=1,B="x,y",C=(D=2))` are parsed on demand by [`parse_struct`].

use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub key: String,
    /// Index for `Key[N]=` lines.
    pub index: Option<usize>,
    pub value: String,
}

#[derive(Debug, Clone, Default)]
pub struct Section {
    pub name: String,
    pub entries: Vec<Entry>,
}

impl Section {
    /// First value of a key (case-insensitive), ignoring indexed entries.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.iter().find(|e| e.index.is_none() && e.key.eq_ignore_ascii_case(key)).map(|e| e.value.as_str())
    }

    /// Every value of a repeated key, in file order (indexed entries included, by order of appearance).
    pub fn get_all(&self, key: &str) -> Vec<&str> {
        self.entries.iter().filter(|e| e.key.eq_ignore_ascii_case(key)).map(|e| e.value.as_str()).collect()
    }

    /// Indexed values `Key[N]=` as (N, value), sorted by N. Plain repeated keys are numbered in order.
    pub fn get_indexed(&self, key: &str) -> Vec<(usize, &str)> {
        let mut next = 0;
        let mut out: Vec<(usize, &str)> = Vec::new();
        for e in self.entries.iter().filter(|e| e.key.eq_ignore_ascii_case(key)) {
            let i = e.index.unwrap_or(next);
            next = i + 1;
            match out.iter_mut().find(|(k, _)| *k == i) {
                Some(slot) => slot.1 = &e.value,
                None => out.push((i, &e.value)),
            }
        }
        out.sort_by_key(|(i, _)| *i);
        out
    }

    fn apply(&mut self, op: char, key: &str, index: Option<usize>, value: &str) {
        let same = |e: &Entry| e.key.eq_ignore_ascii_case(key) && e.index == index;
        match op {
            '+' => {
                if !self.entries.iter().any(|e| same(e) && e.value == value) {
                    self.entries.push(Entry { key: key.into(), index, value: value.into() });
                }
            }
            '-' => self.entries.retain(|e| !(same(e) && e.value == value)),
            '!' => self.entries.retain(|e| !same(e)),
            _ => {
                // Indexed keys overwrite their slot; plain keys accumulate.
                if index.is_some() {
                    if let Some(e) = self.entries.iter_mut().find(|e| same(e)) {
                        e.value = value.into();
                        return;
                    }
                }
                self.entries.push(Entry { key: key.into(), index, value: value.into() });
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Ini {
    pub sections: Vec<Section>,
    index: HashMap<String, usize>,
}

impl Ini {
    pub fn load(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let bytes = std::fs::read(path)?;
        Ok(Self::parse(&decode_text(&bytes)))
    }

    pub fn parse(text: &str) -> Self {
        let mut ini = Ini::default();
        ini.merge_text(text);
        ini
    }

    /// Parses more text on top of this file (later values follow the same +/-/! rules).
    pub fn merge_text(&mut self, text: &str) {
        let mut cur: Option<usize> = None;
        for raw in text.lines() {
            let line = raw.trim_start_matches('\u{feff}').trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                cur = Some(self.section_mut_index(&line[1..line.len() - 1]));
                continue;
            }
            let Some(s) = cur else { continue };
            let Some(eq) = line.find('=') else { continue };
            let (mut key, value) = (line[..eq].trim(), line[eq + 1..].trim_end().trim_start_matches([' ', '\t']));
            let mut op = ' ';
            if let Some(c) = key.chars().next().filter(|c| matches!(c, '+' | '-' | '.' | '!')) {
                op = c;
                key = key[1..].trim();
            }
            let (key, index) = split_index(key);
            self.sections[s].apply(op, key, index, value);
        }
    }

    fn section_mut_index(&mut self, name: &str) -> usize {
        let k = name.trim().to_ascii_lowercase();
        if let Some(&i) = self.index.get(&k) {
            return i;
        }
        self.sections.push(Section { name: name.trim().into(), entries: Vec::new() });
        self.index.insert(k, self.sections.len() - 1);
        self.sections.len() - 1
    }

    pub fn section(&self, name: &str) -> Option<&Section> {
        self.index.get(&name.to_ascii_lowercase()).map(|&i| &self.sections[i])
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.section(section)?.get(key)
    }

    pub fn get_all(&self, section: &str, key: &str) -> Vec<&str> {
        self.section(section).map(|s| s.get_all(key)).unwrap_or_default()
    }
}

fn split_index(key: &str) -> (&str, Option<usize>) {
    if let (Some(o), true) = (key.find('['), key.ends_with(']')) {
        if let Ok(i) = key[o + 1..key.len() - 1].trim().parse() {
            return (key[..o].trim(), Some(i));
        }
    }
    (key, None)
}

/// Shipped ini files are ANSI; a few user-edited ones may be UTF-16 with a BOM.
pub fn decode_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let w: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&w);
    }
    // Big-endian UTF-16 (Subtitles.ini ships like this).
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let w: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&w);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

/// A `Package.Name` class or object reference (e.g. `GameTypes.AWPGameInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassRef {
    pub package: String,
    pub name: String,
}

impl ClassRef {
    /// Parses `Package.Name`, `Class'Package.Name'` or `Package.Name.Map` style strings; returns
    /// None for `None`, empty values and names without a package.
    pub fn parse(s: &str) -> Option<ClassRef> {
        let mut s = s.trim().trim_matches('"');
        if let (Some(a), true) = (s.find('\''), s.ends_with('\'')) {
            s = &s[a + 1..s.len() - 1];
        }
        let (p, n) = s.split_once('.')?;
        if p.is_empty() || n.is_empty() || s.eq_ignore_ascii_case("none") {
            return None;
        }
        Some(ClassRef { package: p.into(), name: n.into() })
    }
}

impl std::fmt::Display for ClassRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.package, self.name)
    }
}

/// Parses a struct literal `(Key=Value,...)` into (key, raw value) pairs. Quoted strings may
/// contain commas and parentheses; nested structs are returned as their raw `( ... )` text.
pub fn parse_struct(s: &str) -> Option<Vec<(String, String)>> {
    let s = s.trim();
    let inner = s.strip_prefix('(')?.strip_suffix(')')?;
    let mut out = Vec::new();
    for part in split_top(inner, ',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (k, v) = part.split_once('=')?;
        out.push((k.trim().to_string(), unquote(v.trim()).to_string()));
    }
    Some(out)
}

/// Splits at `sep` outside quotes and parentheses.
pub fn split_top(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut quoted, mut start) = (0i32, false, 0);
    for (i, c) in s.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth -= 1,
            c if c == sep && !quoted && depth == 0 => {
                out.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

pub fn unquote(s: &str) -> &str {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') { &s[1..s.len() - 1] } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "[URL]\r\nMap=Index.fuk\r\nLocalMap=Startup.fuk\r\n; comment\r\n[Core.System]\r\nPaths=..\\System\\*.u\r\nPaths=..\\Maps\\*.fuk\r\n+Paths=..\\Maps\\*.fuk\r\n+Paths=..\\Extra\\*.u\r\n-Paths=..\\System\\*.u\r\n[Engine.Input]\r\nAliases[1]=(Command=\"Button bAltFire | AltFire\",alias=\"AltFire\")\r\nAliases[0]=(Command=\"Button bFire | Fire\",alias=\"Fire\")\r\nLeftMouse=Fire\r\nHudMode=0\t\r\n[url]\r\nPort=7777\r\n";

    #[test]
    fn sections_keys_and_array_ops() {
        let ini = Ini::parse(SAMPLE);
        assert_eq!(ini.get("url", "map"), Some("Index.fuk"));
        assert_eq!(ini.get("URL", "Port"), Some("7777"), "repeated section merges");
        assert_eq!(ini.get_all("Core.System", "Paths"), vec!["..\\Maps\\*.fuk", "..\\Extra\\*.u"]);
        assert_eq!(ini.get("Engine.Input", "HudMode"), Some("0"), "trailing tab trimmed");
    }

    #[test]
    fn indexed_and_struct_values() {
        let ini = Ini::parse(SAMPLE);
        let a = ini.section("Engine.Input").unwrap().get_indexed("Aliases");
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].0, 0);
        let s = parse_struct(a[0].1).unwrap();
        assert_eq!(s, vec![("Command".to_string(), "Button bFire | Fire".to_string()), ("alias".to_string(), "Fire".to_string())]);
        assert_eq!(ini.get("Engine.Input", "Aliases"), None, "indexed entries are not plain keys");
    }

    #[test]
    fn nested_struct_and_class_refs() {
        let s = parse_struct("(A=1,B=\"x,(y)\",C=(D=2,E=3))").unwrap();
        assert_eq!(s[1].1, "x,(y)");
        assert_eq!(s[2].1, "(D=2,E=3)");
        assert_eq!(ClassRef::parse("GameTypes.AWPGameInfo").unwrap().name, "AWPGameInfo");
        assert_eq!(ClassRef::parse("Class'Engine.Pawn'").unwrap().package, "Engine");
        assert_eq!(ClassRef::parse("None"), None);
        assert_eq!(ClassRef::parse("NoPackage"), None);
    }

    #[test]
    fn clear_and_overwrite() {
        let ini = Ini::parse("[S]\nK=a\nK=b\n!K=\nK=c\nI[2]=x\nI[2]=y\n");
        assert_eq!(ini.get_all("S", "K"), vec!["c"]);
        assert_eq!(ini.section("S").unwrap().get_indexed("I"), vec![(2, "y")]);
    }
}
