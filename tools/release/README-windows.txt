Postal 2 Rust/Bevy - Windows build
===================================

A clean-room Rust/Bevy engine that runs POSTAL 2 using the assets of YOUR OWN installed copy.
This download contains NO game files. You must own POSTAL 2 (e.g. Steam "POSTAL 2").

How to play
-----------
1. Unzip this folder anywhere.
2. Double-click Postal2Rust.exe. It opens a console window (game messages) and the game.
   If the game ever crashes, the console stays open with the error.
3. If your POSTAL 2 install is not found automatically, it asks for the folder (the one
   containing System, Maps, Textures; on Steam: ...\steamapps\common\POSTAL2Complete).
   The choice is remembered in postal2dir.txt.

Options (from a command prompt in this folder):
  Postal2Rust.exe --change-folder     ask for the POSTAL 2 folder again
  Postal2Rust.exe --windowed          play in a window instead of fullscreen
  Postal2Rust.exe --map suburbs-1     start on a specific map

Requirements: Windows 10/11 64-bit, a GPU with Vulkan or DirectX 12.
Keep any .dll files shipped in this folder next to Postal2Rust.exe.

Status: work in progress. Monday to Friday and Apocalypse Weekend are playable start to
finish, but expect visual and AI differences from the original game.

Source: https://github.com/lukaisimcard/postal2rust
POSTAL 2 is (c) Running With Scissors. This project is unaffiliated with them.
