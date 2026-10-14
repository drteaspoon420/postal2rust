Postal 2 Rust/Bevy - Linux build (x86_64)
==========================================

A clean-room Rust/Bevy engine that runs POSTAL 2 using the assets of YOUR OWN installed copy.
This download contains NO game files. You must own POSTAL 2 (e.g. Steam "POSTAL 2").

The Linux build is new and less tested than the Windows one. Please report problems.

How to play
-----------
1. Extract this archive anywhere:   tar xzf Postal2Rust-linux-x86_64.tar.gz
2. Run it from a terminal (so you can see the game messages):
       cd Postal2Rust-linux-x86_64
       ./Postal2Rust
3. The POSTAL 2 install is searched in the usual Steam folders:
       ~/.local/share/Steam/steamapps/common/POSTAL2Complete
       ~/.steam/steam/steamapps/common/POSTAL2Complete
       Flatpak Steam (~/.var/app/com.valvesoftware.Steam/...)
       every Steam library listed in steamapps/libraryfolders.vdf
   If it is not found, it asks for the folder (the one containing System, Maps, Textures)
   with zenity or kdialog when installed, otherwise in the terminal.
   The choice is remembered in postal2dir.txt next to Postal2Rust.

   Getting the game files: in Steam, install POSTAL 2 normally (the Windows depot through
   Steam Play / Proton works fine). Only the data folders are read; nothing in the install
   is modified. GOG installs (e.g. through Heroic or Lutris) work too, point it at the
   folder containing System.

Options:
  ./Postal2Rust --change-folder       ask for the POSTAL 2 folder again
  ./Postal2Rust --windowed            play in a window instead of fullscreen
  ./Postal2Rust --game /path/to/POSTAL2Complete
  ./Postal2Rust --map suburbs-1       start on a specific map

Requirements: 64-bit glibc 2.35+ distribution (Ubuntu 22.04 or newer and equivalents),
a Vulkan capable GPU and driver (Mesa RADV/ANV, NVIDIA proprietary), ALSA or PipeWire/
PulseAudio (through its ALSA plugin) for sound, X11 or Wayland.
Packages commonly needed: libvulkan1, libasound2, libudev1, libxkbcommon0 (usually
already installed on desktop systems).

File names: POSTAL 2 data comes from Windows with mixed upper/lower case names. The game
looks files up case-insensitively, so no renaming is needed.

Saves and settings go to ~/.local/share/postal2rust (or $XDG_DATA_HOME/postal2rust).

Status: work in progress. Monday to Friday and Apocalypse Weekend are playable start to
finish, but expect visual and AI differences from the original game.

Source: https://github.com/lukaisimcard/postal2rust
POSTAL 2 is (c) Running With Scissors. This project is unaffiliated with them.
