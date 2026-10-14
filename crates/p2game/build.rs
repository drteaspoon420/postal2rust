//! Embeds the app icon (res/app.ico, original rust-gear art from tools/release/make_icon.ps1) into
//! the Windows exe. Uses llvm-windres / windres from PATH; without one the exe just has no icon.

fn main() {
    println!("cargo:rerun-if-changed=res/app.rc");
    println!("cargo:rerun-if-changed=res/app.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let rc = manifest.join("res").join("app.rc");
    let res = out.join("app_res.o");
    let target = std::env::var("TARGET").unwrap_or_default();
    let fmt = if target.contains("msvc") { "res" } else { "coff" };
    let res = if fmt == "res" { out.join("app.res") } else { res };
    for tool in ["llvm-windres", "windres", "x86_64-w64-mingw32-windres"] {
        let ok = std::process::Command::new(tool)
            .arg("-i")
            .arg(&rc)
            .arg("-o")
            .arg(&res)
            .arg("-O")
            .arg(fmt)
            .arg("-I")
            .arg(manifest.join("res"))
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            println!("cargo:rustc-link-arg-bins={}", res.display());
            return;
        }
    }
    println!("cargo:warning=no windres found: p2game.exe built without an icon");
}
