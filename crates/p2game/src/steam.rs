//! Steam library and installed-app discovery, shared by both games. Read-only:
//! manifests locate an install; the caller validates the actual game files.
use std::path::{Component, Path, PathBuf};

/// Valve KeyValues tokens, including escaped Windows paths and // comments.
fn tokens(input: &str) -> Vec<String> {
    let mut chars = input.chars().peekable();
    let mut out = Vec::new();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() || c == '{' || c == '}' => {}
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '"' => {
                let mut value = String::new();
                while let Some(c) = chars.next() {
                    if c == '"' {
                        break;
                    }
                    if c == '\\' {
                        match chars.next() {
                            Some('\\') => value.push('\\'),
                            Some('"') => value.push('"'),
                            Some(c) => {
                                value.push('\\');
                                value.push(c);
                            }
                            None => break,
                        }
                    } else {
                        value.push(c);
                    }
                }
                out.push(value);
            }
            c => {
                let mut value = String::from(c);
                while chars
                    .peek()
                    .is_some_and(|c| !c.is_whitespace() && *c != '{' && *c != '}')
                {
                    value.push(chars.next().unwrap());
                }
                out.push(value);
            }
        }
    }
    out
}
fn field(input: &str, key: &str) -> Option<String> {
    tokens(input)
        .windows(2)
        .find(|p| p[0].eq_ignore_ascii_case(key))
        .map(|p| p[1].clone())
}
pub fn library_paths(input: &str) -> Vec<PathBuf> {
    tokens(input)
        .windows(2)
        .filter(|p| {
            p[0].eq_ignore_ascii_case("path")
                || (p[0].parse::<u32>().is_ok()
                    && (p[1].starts_with('/') || p[1].contains(':') || p[1].starts_with("\\\\")))
        })
        .map(|p| PathBuf::from(&p[1]))
        .collect()
}
fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    let path = path.canonicalize().unwrap_or(path);
    if !paths.contains(&path) {
        paths.push(path);
    }
}
pub fn libraries() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for name in ["STEAM_DIR", "STEAM_PATH", "SteamPath"] {
        if let Some(p) = std::env::var_os(name).filter(|p| !p.is_empty()) {
            push_unique(&mut roots, PathBuf::from(p));
        }
    }
    #[cfg(windows)]
    {
        if let Ok(out) = std::process::Command::new("reg")
            .args(["query", r"HKCU\Software\Valve\Steam", "/v", "SteamPath"])
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            if let Some(path) = text
                .lines()
                .find(|l| l.contains("SteamPath"))
                .and_then(|l| l.split("REG_SZ").nth(1))
            {
                push_unique(&mut roots, PathBuf::from(path.trim()));
            }
        }
        for variable in ["ProgramFiles(x86)", "ProgramFiles"] {
            if let Some(p) = std::env::var_os(variable) {
                push_unique(&mut roots, PathBuf::from(p).join("Steam"));
            }
        }
    }
    #[cfg(unix)]
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        #[cfg(target_os = "macos")]
        push_unique(&mut roots, home.join("Library/Application Support/Steam"));
        #[cfg(all(unix, not(target_os = "macos")))]
        for rel in [
            ".local/share/Steam",
            ".steam/steam",
            ".steam/root",
            ".var/app/com.valvesoftware.Steam/.local/share/Steam",
            "snap/steam/common/.local/share/Steam",
        ] {
            push_unique(&mut roots, home.join(rel));
        }
    }
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        push_unique(&mut roots, PathBuf::from(data).join("Steam"));
    }
    let mut libraries = roots.clone();
    for root in roots {
        if let Ok(vdf) =
            std::fs::read_to_string(upkg::ci::join(&root, "steamapps/libraryfolders.vdf"))
        {
            for p in library_paths(&vdf) {
                push_unique(&mut libraries, p);
            }
        }
    }
    libraries
}
pub fn app_in_library(library: &Path, app_id: u32) -> Option<PathBuf> {
    let apps = upkg::ci::join(library, "steamapps");
    let manifest =
        std::fs::read_to_string(upkg::ci::join(&apps, &format!("appmanifest_{app_id}.acf")))
            .ok()?;
    if field(&manifest, "appid")?.parse::<u32>().ok()? != app_id {
        return None;
    }
    let name = field(&manifest, "installdir")?;
    let name = Path::new(&name);
    if name.as_os_str().is_empty() || !name.components().all(|c| matches!(c, Component::Normal(_)))
    {
        return None;
    }
    let install = upkg::ci::join(&upkg::ci::join(&apps, "common"), name);
    install.is_dir().then_some(install)
}
pub fn installs(app_id: u32) -> Vec<PathBuf> {
    libraries()
        .iter()
        .filter_map(|p| app_in_library(p, app_id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modern_and_legacy_libraries_preserve_spaces_and_escaped_paths() {
        let paths = library_paths(
            r#""libraryfolders" { "0" { "path" "C:\\Steam" "apps" { "42910" "123" } } "1" "/games/Steam Library" "2" { "path" "D:\\Steam Library" } } // ignored "path" "/bad""#,
        );
        assert_eq!(
            paths,
            vec![
                PathBuf::from(r"C:\Steam"),
                PathBuf::from("/games/Steam Library"),
                PathBuf::from(r"D:\Steam Library")
            ]
        );
    }
    #[test]
    fn manifest_uses_installdir_and_rejects_escape_or_wrong_app() {
        let root = std::env::temp_dir().join(format!("p2-steam-manifest-{}", std::process::id()));
        std::fs::create_dir_all(root.join("steamapps/common/A Custom Install")).unwrap();
        let file = root.join("steamapps/appmanifest_42910.acf");
        std::fs::write(
            &file,
            r#""AppState" { "appid" "42910" "installdir" "A Custom Install" }"#,
        )
        .unwrap();
        assert_eq!(
            app_in_library(&root, 42910),
            Some(root.join("steamapps/common/A Custom Install"))
        );
        for (id, path) in [
            ("42910", "../outside"),
            ("42910", "/outside"),
            ("7", "A Custom Install"),
        ] {
            std::fs::write(
                &file,
                format!(r#""AppState" {{ "appid" "{id}" "installdir" "{path}" }}"#),
            )
            .unwrap();
            assert_eq!(app_in_library(&root, 42910), None);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
