@echo off
rem Change the POSTAL 2 install folder used by Postal2Rust.
rem Opens the folder picker, saves the choice in postal2dir.txt next to Postal2Rust.exe, then starts the game.
cd /d "%~dp0"
if not exist "Postal2Rust.exe" (
  echo Postal2Rust.exe was not found next to this file. Keep postal2-dir.bat in the game folder.
  pause
  exit /b 1
)
if exist postal2dir.txt (
  echo Current POSTAL 2 folder:
  type postal2dir.txt
  echo.
)
echo Choose your POSTAL 2 folder (the one containing System, Maps and Textures).
start "" "Postal2Rust.exe" --change-folder %*
