//! Boot configuration: which ini files the game reads, and what they select.
//!
//! The game executable name (`[URL] EXEName`, `Postal2.exe`) gives the main ini name
//! (`Postal2.ini`), and the user ini is `User.ini`. If either is missing, the shipped templates
//! `Default.ini` / `DefUser.ini` are used instead. Values here are what the original engine reads
//! at startup: the default GameInfo class, the entry and startup maps, the player pawn class, the
//! console class, the package search paths, the package remap table, and input bindings.

use crate::ini::{ClassRef, Ini, parse_struct};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct InputAlias {
    pub alias: String,
    pub command: String,
}

#[derive(Debug, Clone)]
pub struct BootConfig {
    pub system_dir: PathBuf,
    pub main_ini_path: PathBuf,
    pub user_ini_path: PathBuf,
    pub main: Ini,
    pub user: Ini,
    pub default_game: Option<ClassRef>,
    pub default_server_game: Option<ClassRef>,
    /// The persistent entry level, loaded once before any other map.
    pub entry_map: String,
    /// `[URL] LocalMap`: the first playable map (the main menu level).
    pub startup_map: String,
    /// `[URL] Map`: default map for servers/URLs without a map.
    pub default_map: String,
    pub map_ext: String,
    pub player_name: String,
    pub player_class: Option<ClassRef>,
    pub console: Option<ClassRef>,
    pub paths: Vec<String>,
    /// `[PackageRemap]`: old package name to the packages its contents moved into.
    pub package_remap: Vec<(String, String)>,
    pub aliases: Vec<InputAlias>,
    /// Key name to command string (`LeftMouse=Fire`), only non-empty bindings.
    pub bindings: Vec<(String, String)>,
}

fn pick(system: &Path, primary: &str, fallback: &str) -> PathBuf {
    let p = system.join(primary);
    if p.is_file() { p } else { system.join(fallback) }
}

impl BootConfig {
    pub fn load(system_dir: &Path) -> std::io::Result<Self> {
        // The engine names its main ini after the executable; read the shipped default first to
        // learn the executable name, then prefer the live file.
        let exe = Ini::load(system_dir.join("Default.ini"))
            .ok()
            .and_then(|i| i.get("URL", "EXEName").map(|s| s.to_string()))
            .unwrap_or_else(|| "Postal2.exe".into());
        let stem = Path::new(&exe).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Postal2".into());
        let main_ini_path = pick(system_dir, &format!("{stem}.ini"), "Default.ini");
        let user_ini_path = pick(system_dir, "User.ini", "DefUser.ini");
        let main = Ini::load(&main_ini_path)?;
        let user = Ini::load(&user_ini_path)?;
        Ok(Self::from_inis(system_dir.to_path_buf(), main_ini_path, user_ini_path, main, user))
    }

    pub fn from_inis(system_dir: PathBuf, main_ini_path: PathBuf, user_ini_path: PathBuf, main: Ini, user: Ini) -> Self {
        let map_stem = |s: Option<&str>, d: &str| {
            let s = s.unwrap_or(d);
            Path::new(s).file_stem().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| d.into())
        };
        let mut aliases = Vec::new();
        let mut bindings = Vec::new();
        if let Some(sec) = user.section("Engine.Input") {
            for (_, v) in sec.get_indexed("Aliases") {
                let Some(fields) = parse_struct(v) else { continue };
                let get = |k: &str| fields.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone()).unwrap_or_default();
                let (alias, command) = (get("Alias"), get("Command"));
                if !alias.eq_ignore_ascii_case("None") && !alias.is_empty() {
                    aliases.push(InputAlias { alias, command });
                }
            }
            for e in sec.entries.iter().filter(|e| e.index.is_none() && !e.value.trim().is_empty()) {
                bindings.push((e.key.clone(), e.value.trim().to_string()));
            }
        }
        let package_remap = main
            .section("PackageRemap")
            .map(|s| s.entries.iter().map(|e| (e.key.clone(), e.value.clone())).collect())
            .unwrap_or_default();
        BootConfig {
            default_game: main.get("Engine.Engine", "DefaultGame").and_then(ClassRef::parse),
            default_server_game: main.get("Engine.Engine", "DefaultServerGame").and_then(ClassRef::parse),
            console: main.get("Engine.Engine", "Console").and_then(ClassRef::parse),
            entry_map: "Entry".into(),
            startup_map: map_stem(main.get("URL", "LocalMap"), "Entry"),
            default_map: map_stem(main.get("URL", "Map"), "Index"),
            map_ext: main.get("URL", "MapExt").unwrap_or("fuk").to_string(),
            player_name: user.get("DefaultPlayer", "Name").unwrap_or("Player").to_string(),
            player_class: user.get("DefaultPlayer", "Class").and_then(ClassRef::parse),
            paths: main.get_all("Core.System", "Paths").into_iter().map(String::from).collect(),
            package_remap,
            aliases,
            bindings,
            system_dir,
            main_ini_path,
            user_ini_path,
            main,
            user,
        }
    }

    /// The ini file backing a class `config(Name)`: System is the main ini, User is User.ini.
    pub fn config_ini(&self, config_name: &str) -> Option<&Ini> {
        match config_name.to_ascii_lowercase().as_str() {
            "system" => Some(&self.main),
            "user" => Some(&self.user),
            _ => None,
        }
    }

    /// Packages to try for a reference into `package`: itself, then its remap targets.
    pub fn package_candidates(&self, package: &str) -> Vec<String> {
        let mut v = vec![package.to_string()];
        for (from, to) in &self.package_remap {
            if from.eq_ignore_ascii_case(package) && !v.iter().any(|x| x.eq_ignore_ascii_case(to)) {
                v.push(to.clone());
            }
        }
        v
    }

    /// The command bound to a key, with aliases expanded one level (`LeftMouse=Fire` gives
    /// `Button bFire | Fire`).
    pub fn binding(&self, key: &str) -> Option<String> {
        let cmd = self.bindings.iter().find(|(k, _)| k.eq_ignore_ascii_case(key))?.1.clone();
        let expanded: Vec<String> = cmd
            .split('|')
            .map(|part| {
                let p = part.trim();
                match self.aliases.iter().find(|a| a.alias.eq_ignore_ascii_case(p)) {
                    Some(a) => a.command.clone(),
                    None => p.to_string(),
                }
            })
            .collect();
        Some(expanded.join(" | "))
    }

    pub fn print(&self) {
        let opt = |c: &Option<ClassRef>| c.as_ref().map(|c| c.to_string()).unwrap_or_else(|| "(none)".into());
        println!("main ini      : {}", self.main_ini_path.display());
        println!("user ini      : {}", self.user_ini_path.display());
        println!("DefaultGame   : {}", opt(&self.default_game));
        println!("ServerGame    : {}", opt(&self.default_server_game));
        println!("Console       : {}", opt(&self.console));
        println!("entry map     : {}.{}", self.entry_map, self.map_ext);
        println!("startup map   : {}.{} (LocalMap)", self.startup_map, self.map_ext);
        println!("default map   : {}.{} (URL Map)", self.default_map, self.map_ext);
        println!("player        : {} as {}", self.player_name, opt(&self.player_class));
        println!("paths         : {}", self.paths.join(" "));
        println!("package remap : {}", self.package_remap.iter().map(|(a, b)| format!("{a}->{b}")).collect::<Vec<_>>().join(" "));
        println!("input         : {} aliases, {} bound keys", self.aliases.len(), self.bindings.len());
        for k in ["W", "S", "A", "D", "LeftMouse", "RightMouse", "Space", "E", "Escape"] {
            if let Some(b) = self.binding(k) {
                println!("  {k:<10} = {b}");
            }
        }
    }
}

/// The shipped ini files as the VM's base config layer (`uscript::config::ConfigSource`):
/// `Postal2.ini` and `User.ini` come from the boot inis, any other `Name.ini` is read from the
/// System directory on first use.
pub struct IniConfig {
    main: Ini,
    user: Ini,
    system_dir: PathBuf,
    others: std::cell::RefCell<std::collections::HashMap<String, Option<Ini>>>,
}

impl IniConfig {
    pub fn new(cfg: &BootConfig) -> Self {
        Self { main: cfg.main.clone(), user: cfg.user.clone(), system_dir: cfg.system_dir.clone(), others: Default::default() }
    }
}

fn indexed(ini: &Ini, section: &str, key: &str) -> Vec<(usize, String)> {
    ini.section(section).map(|s| s.get_indexed(key).into_iter().map(|(i, v)| (i, v.to_string())).collect()).unwrap_or_default()
}

impl uscript::config::ConfigSource for IniConfig {
    fn values(&self, file: &str, section: &str, key: &str) -> Vec<(usize, String)> {
        let f = file.to_ascii_lowercase();
        if matches!(f.as_str(), "postal2.ini" | "system.ini" | "default.ini") {
            return indexed(&self.main, section, key);
        }
        if matches!(f.as_str(), "user.ini" | "defuser.ini") {
            return indexed(&self.user, section, key);
        }
        let mut others = self.others.borrow_mut();
        let ini = others.entry(f).or_insert_with(|| Ini::load(self.system_dir.join(file)).ok());
        ini.as_ref().map(|i| indexed(i, section, key)).unwrap_or_default()
    }
}
