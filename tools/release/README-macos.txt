Postal 2 Rust/Bevy - macOS build (universal: Apple Silicon and Intel)
=====================================================================

A clean-room Rust/Bevy engine that runs POSTAL 2 using the assets of YOUR OWN installed copy.
This download contains NO game files. You must own POSTAL 2 (e.g. Steam "POSTAL 2").

The macOS build is new and less tested than the Windows one. Please report problems.

How to play
-----------
1. Extract the archive (double-click it, or: tar xzf Postal2Rust-macos-universal.tar.gz).
2. The program is not signed by Apple, so macOS blocks it the first time. In Terminal:
       cd ~/Downloads/Postal2Rust-macos-universal     (wherever you extracted it)
       xattr -dr com.apple.quarantine .
       ./Postal2Rust
   (Alternatively: right-click Postal2Rust in Finder, choose Open, then confirm Open.)
3. The POSTAL 2 install is searched in the Steam folder:
       ~/Library/Application Support/Steam/steamapps/common/POSTAL2Complete
   and in every Steam library listed in steamapps/libraryfolders.vdf.
   If it is not found, a folder picker asks for it (the folder containing System, Maps,
   Textures). The choice is remembered in postal2dir.txt next to Postal2Rust.

   Getting the game files: the engine needs the data folders of the PC (Windows) version:
   System, Maps, Textures, StaticMeshes, Animations, Sounds, Music, Movies, KarmaData. The
   simplest way is to copy the whole POSTAL2Complete folder from a Windows install (Steam
   or GOG) to your Mac and point the picker at it. If your Mac Steam install has a folder
   with System/Postal2.ini inside it, that works as well.

Optional Magicka integration
----------------------------
Magicka is detected from Steam, including custom libraries, and enabled automatically.
The first launch copies required Magicka XNB/audio assets to assets/magicka/Content
inside this extracted Postal2Rust folder. No conversion tools or developer checkout
are required. Original Steam files are not modified. With no Magicka found, normal
POSTAL 2 starts. Use --no-magicka to choose normal POSTAL 2 explicitly.
Use --magicka-content /path/to/Magicka/Content for a manual install, or
--prepare-assets to prepare files without opening the game. Place this release in
a folder you can write to so the first-launch asset preparation can finish.

Options:
  ./Postal2Rust --change-folder       ask for the POSTAL 2 folder again
  ./Postal2Rust --windowed            play in a window instead of fullscreen
  ./Postal2Rust --game "/path/to/POSTAL2Complete"
  ./Postal2Rust --map suburbs-1       start on a specific map

Requirements: macOS 11 Big Sur or newer, a Metal capable Mac (any Apple Silicon Mac,
most Intel Macs from 2012 on).

File names: POSTAL 2 data comes from Windows with mixed upper/lower case names. The game
looks files up case-insensitively, so case-sensitive volumes work too.

Saves and settings go to ~/.local/share/postal2rust (or $XDG_DATA_HOME/postal2rust).

Status: work in progress. Monday to Friday and Apocalypse Weekend are playable start to
finish, but expect visual and AI differences from the original game.

Source: https://github.com/lukaisimcard/postal2rust
POSTAL 2 is (c) Running With Scissors. This project is unaffiliated with them.
