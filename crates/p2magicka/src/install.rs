//! Prepare user-owned native XNB assets beside Postal2Rust. This adapter reads
//! XNB directly; it does not require the rewrite viewer's glTF/PNG export tree.
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub fn content_root(source: &Path) -> Option<PathBuf> {
    [source.to_path_buf(), source.join("Content")]
        .into_iter()
        .find(|p| {
            crate::magicka_runtime::play_content::resolve(
                p,
                Path::new("Data/Characters/Wizard.xnb"),
            )
            .is_some_and(|f| f.is_file())
        })
}
pub fn project_directory(executable: &Path) -> PathBuf {
    let beside = executable.parent().unwrap_or(Path::new("."));
    for parent in beside.ancestors() {
        if parent.join("crates/p2game/Cargo.toml").is_file() {
            return parent.to_path_buf();
        }
    }
    beside.to_path_buf()
}
pub fn prepared_content(directory: &Path) -> Option<PathBuf> {
    let root = directory.join("assets/magicka/Content");
    root.join(".postal2rust-assets-v1")
        .is_file()
        .then(|| content_root(&root))
        .flatten()
}
fn copy_tree(source: &Path, destination: &Path, copied: &mut usize) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&from, &to, copied)?;
        } else if kind.is_file() || (kind.is_symlink() && from.is_file()) {
            copy_file(&from, &to, copied)?;
        }
    }
    Ok(())
}
fn copy_file(from: &Path, to: &Path, copied: &mut usize) -> io::Result<()> {
    let original = fs::metadata(from)?;
    let fresh = fs::metadata(to).is_ok_and(|m| {
        m.len() == original.len()
            && m.modified()
                .ok()
                .zip(original.modified().ok())
                .is_some_and(|(a, b)| a >= b)
    });
    if fresh {
        return Ok(());
    }
    let temp = to.with_file_name(format!(
        ".{}.p2import-{}",
        to.file_name().unwrap().to_string_lossy(),
        std::process::id()
    ));
    fs::copy(from, &temp)?;
    // Windows rename cannot replace an existing file.
    #[cfg(windows)]
    {
        let mut permissions = fs::metadata(&temp)?.permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&temp, permissions)?;
        if to.exists() {
            let mut permissions = fs::metadata(to)?.permissions();
            permissions.set_readonly(false);
            fs::set_permissions(to, permissions)?;
            fs::remove_file(to)?;
        }
    }
    fs::rename(&temp, to)?;
    *copied += 1;
    Ok(())
}
/// Content is copied in its original compressed-XNB/audio-bank formats. The
/// runtime's readers decode it in memory, so no Python/export tools are needed.
pub fn prepare(source: &Path, directory: &Path) -> io::Result<PathBuf> {
    let source = content_root(source).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Magicka Content/Data/Characters/Wizard.xnb is missing",
        )
    })?;
    let destination = directory.join("assets/magicka/Content");
    fs::create_dir_all(&destination)?;
    let marker = destination.join(".postal2rust-assets-v1");
    if source.canonicalize()? == destination.canonicalize()? {
        if !marker.is_file() { fs::write(marker, "native-xnb-v1\n")?; }
        return Ok(destination);
    }
    // An interrupted import must be retried rather than treated as complete.
    match fs::remove_file(&marker) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    println!("Preparing Magicka assets in {}", destination.display());
    // The hosted wizard needs gameplay, models, effects, HUD and XACT banks.
    // Magicka campaign levels, source FBX and localized campaigns are not used.
    let mut copied = 0;
    for entry in fs::read_dir(&source)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if [
                "audio",
                "data",
                "effecttextures",
                "effects",
                "models",
                "shaders",
                "ui",
            ]
            .contains(&name.as_str())
            {
                copy_tree(
                    &entry.path(),
                    &destination.join(entry.file_name()),
                    &mut copied,
                )?;
            }
        } else if entry
            .path()
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("xnb"))
        {
            let to = destination.join(entry.file_name());
            // Small root effect assets; use the same incremental copy path.
            copy_file(&entry.path(), &to, &mut copied)?;
        }
    }
    if content_root(&destination).is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Prepared Magicka assets are incomplete",
        ));
    }
    fs::write(
        destination.join(".postal2rust-assets-v1"),
        format!("native-xnb-v1\nsource={}\n", source.display()),
    )?;
    println!("Magicka assets ready ({copied} files copied; existing files reused)");
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn assets_are_local_incremental_and_keep_native_xnb_format() {
        let root = std::env::temp_dir().join(format!("p2-magicka-import-{}", std::process::id()));
        let source = root.join("Steam Library/common/Magicka/Content");
        fs::create_dir_all(source.join("Data/Characters")).unwrap();
        fs::create_dir_all(source.join("Models")).unwrap();
        fs::create_dir_all(source.join("Levels")).unwrap();
        fs::write(
            source.join("Data/Characters/Wizard.xnb"),
            b"XNB-native-test",
        )
        .unwrap();
        fs::write(source.join("Models/Wizard.xnb"), b"model").unwrap();
        fs::write(source.join("Levels/not-needed.lvl"), b"campaign").unwrap();
        let project = root.join("Postal2Rust");
        assert_eq!(prepared_content(&project), None);
        let dest = prepare(source.parent().unwrap(), &project).unwrap();
        assert_eq!(
            fs::read(dest.join("Data/Characters/Wizard.xnb")).unwrap(),
            b"XNB-native-test"
        );
        assert!(!dest.join("Levels").exists());
        assert_eq!(prepared_content(&project), Some(dest.clone()));
        let before = fs::metadata(dest.join("Models/Wizard.xnb"))
            .unwrap()
            .modified()
            .unwrap();
        prepare(&source, &project).unwrap();
        assert_eq!(
            fs::metadata(dest.join("Models/Wizard.xnb"))
                .unwrap()
                .modified()
                .unwrap(),
            before
        );
        fs::write(source.join("Models/Wizard.xnb"), b"updated-model").unwrap();
        prepare(&source, &project).unwrap();
        assert_eq!(
            fs::read(dest.join("Models/Wizard.xnb")).unwrap(),
            b"updated-model"
        );
        assert!(prepare(&root.join("missing"), &project).is_err());
        assert!(project_directory(&project.join("Postal2Rust.exe")).starts_with(&project));
        fs::remove_dir_all(root).unwrap();
    }
}
