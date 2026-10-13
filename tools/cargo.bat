@echo off
rem Runs cargo with the toolchain installed under F:\Tools (system PATH is untouched).
rem Usage: tools\cargo.bat <cargo args...>
set "RUSTUP_HOME=F:\Tools\rustup"
set "CARGO_HOME=F:\Tools\cargo"
rem Toolchain: x86_64-pc-windows-gnullvm (LLVM/lld, no MSVC or GCC needed). llvm-mingw supplies
rem the clang linker driver, lld, dlltool and the mingw-w64 CRT that this target links against.
set "RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-gnullvm"
set "PATH=F:\Tools\cargo\bin;F:\Tools\llvm-mingw-20260922-ucrt-x86_64\bin;%PATH%"
rem Respect a caller-provided target dir (parallel agents use target-<name> to avoid lock contention).
if not defined CARGO_TARGET_DIR set "CARGO_TARGET_DIR=F:\postal2rust\target"
cd /d F:\postal2rust
cargo %*
