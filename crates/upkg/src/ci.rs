//! Case-insensitive file lookup. POSTAL 2 data was made on Windows: scripts, inis and packages
//! name files with whatever case they like (`engine.u` for `Engine.u`, `..\Maps\*.fuk`, `user.ini`).
//! On case-sensitive file systems (Linux, some macOS volumes) a plain `join` misses those, so
//! [`join`] resolves each component against the directory listing when the exact path is absent.
//! On Windows it is a plain join.

use std::path::{Path, PathBuf};

/// `base` joined with `rel` (separators `/` or `\`), matching each component of `rel`
/// case-insensitively when the exact-case path does not exist. Components that cannot be
/// matched are kept as written, so the result is always a usable (if missing) path.
pub fn join(base: impl AsRef<Path>, rel: impl AsRef<Path>) -> PathBuf {
    let base = base.as_ref();
    let rel = rel.as_ref();
    #[cfg(windows)]
    {
        base.join(rel)
    }
    #[cfg(not(windows))]
    {
        use std::path::Component;
        let s = rel.to_string_lossy().replace('\\', "/");
        let rel = Path::new(&s);
        let direct = base.join(rel);
        if direct.exists() {
            return direct;
        }
        let mut cur = PathBuf::from(base);
        for c in rel.components() {
            match c {
                Component::Normal(n) => {
                    let exact = cur.join(n);
                    if exact.exists() {
                        cur = exact;
                        continue;
                    }
                    let want = n.to_string_lossy().to_lowercase();
                    let found = std::fs::read_dir(&cur).ok().and_then(|rd| {
                        rd.flatten().map(|e| e.file_name()).find(|f| f.to_string_lossy().to_lowercase() == want)
                    });
                    cur = cur.join(found.as_deref().unwrap_or(n));
                }
                other => cur.push(other.as_os_str()),
            }
        }
        cur
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_mixed_case() {
        let dir = std::env::temp_dir().join(format!("upkg_ci_{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join("System"));
        std::fs::write(dir.join("System").join("Engine.u"), b"x").unwrap();
        let p = super::join(&dir, "system\\engine.u");
        assert!(p.is_file(), "{}", p.display());
        let p = super::join(&dir, "System/../System/ENGINE.U");
        assert!(p.is_file(), "{}", p.display());
        assert!(!super::join(&dir, "System/missing.u").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
