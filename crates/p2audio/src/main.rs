//! p2audio: inspect, export and play POSTAL 2 sounds from the user's own installation.
//!
//! ```text
//! p2audio [--game DIR] --list <package|map>            list sounds (class, codec, rate, length)
//! p2audio [--game DIR] --export <pkg.[group.]name> out.wav   decode to 16-bit PCM WAV
//! p2audio [--game DIR] --play <pkg.[group.]name>        play through the default audio device
//! p2audio [--game DIR] --play Music\<file>.ogg          play a loose music file
//! ```
//! ProceduralSound/SoundGroup references are followed to a concrete Sound (first group entry).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use upkg::library::{Library, Obj};
use upkg::sound::{Pcm, SoundKind, codec, parse_wav, read_sound, sound_exports};
use upkg::{ObjRef, Package};

const DEFAULT_GAME: &str = r"G:\SteamLibrary\steamapps\common\POSTAL2Complete";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let game = get("--game").map(PathBuf::from).or_else(|| std::env::var_os("POSTAL2_DIR").map(PathBuf::from)).unwrap_or_else(|| DEFAULT_GAME.into());
    let res = if let Some(p) = get("--list") {
        list(&game, &p)
    } else if let Some(name) = get("--export") {
        let out = args.iter().position(|a| a == "--export").and_then(|i| args.get(i + 2)).cloned().unwrap_or_else(|| "out.wav".into());
        export(&game, &name, &out)
    } else if let Some(name) = get("--play") {
        play(&game, &name)
    } else {
        Err("usage: p2audio [--game DIR] --list <pkg> | --export <pkg.name> out.wav | --play <pkg.name | file.ogg>".into())
    };
    if let Err(e) = res {
        eprintln!("p2audio: {e}");
        std::process::exit(1);
    }
}

type R<T> = Result<T, String>;

fn open_pkg(lib: &Library, game: &Path, name: &str) -> R<Arc<Package>> {
    let p = Path::new(name);
    if p.is_file() {
        let pkg = Arc::new(Package::open(p).map_err(|e| e.to_string())?);
        lib.insert(pkg.clone());
        return Ok(pkg);
    }
    lib.package(name).ok_or_else(|| format!("package '{name}' not found under {}", game.display()))
}

fn list(game: &Path, name: &str) -> R<()> {
    let lib = Library::new(game).map_err(|e| e.to_string())?;
    let pkg = open_pkg(&lib, game, name)?;
    let mut total = 0f64;
    let idx = sound_exports(&pkg);
    for &i in &idx {
        let path = pkg.full_path(ObjRef::Export(i));
        match read_sound(&pkg, i) {
            Ok(s) if s.kind == SoundKind::Sound => {
                let detail = match parse_wav(s.data) {
                    Ok(w) => {
                        let secs = s.decode_pcm().map(|p| p.duration_secs()).unwrap_or(0.0);
                        total += secs;
                        format!("{} {}-bit {}ch {:5} Hz {:7.2}s", codec::name(w.format_tag), w.bits_per_sample, w.channels, w.sample_rate, secs)
                    }
                    Err(e) => format!("{:?} ({e})", s.container()),
                };
                println!("{path:<60} {detail}");
            }
            Ok(s) => {
                let refs: Vec<String> = if s.kind == SoundKind::Group {
                    s.group_sounds().into_iter().map(|r| pkg.full_path(r)).collect()
                } else {
                    s.base_sound().map(|r| pkg.full_path(r)).into_iter().collect()
                };
                println!("{path:<60} {} -> {}", s.class, refs.join(", "));
            }
            Err(e) => println!("{path:<60} ERROR {e}"),
        }
    }
    println!("{} sound objects, {:.1}s of audio", idx.len(), total);
    Ok(())
}

/// Finds a sound by "pkg.name" or "pkg.group.name" (groups optional, case-insensitive).
fn find(lib: &Library, game: &Path, spec: &str) -> R<Obj> {
    let (pkg_name, rest) = spec.split_once('.').ok_or("expected <package>.<sound>")?;
    let pkg = open_pkg(lib, game, pkg_name)?;
    let want = spec.to_lowercase();
    let leaf = rest.rsplit('.').next().unwrap_or(rest).to_lowercase();
    let cands = sound_exports(&pkg);
    let exact = cands.iter().copied().find(|&i| pkg.full_path(ObjRef::Export(i)).to_lowercase() == want);
    let by_leaf = || cands.iter().copied().find(|&i| pkg.name(pkg.exports[i].name).eq_ignore_ascii_case(&leaf));
    let export = exact.or_else(by_leaf).ok_or_else(|| format!("no sound '{rest}' in {pkg_name}"))?;
    Ok(Obj { pkg, export })
}

/// Follows ProceduralSound.BaseSound / SoundGroup.Sounds[0] until a sound with file data.
fn resolve_playable(lib: &Library, mut obj: Obj) -> R<(String, Pcm)> {
    for _ in 0..16 {
        let s = read_sound(&obj.pkg, obj.export).map_err(|e| e.to_string())?;
        let path = obj.pkg.full_path(ObjRef::Export(obj.export));
        let next = match s.kind {
            SoundKind::Procedural => s.base_sound(),
            SoundKind::Group => s.group_sounds().into_iter().find(|r| *r != ObjRef::Null),
            _ => {
                let pcm = s.decode_pcm().map_err(|e| format!("{path}: {e}"))?;
                return Ok((path, pcm));
            }
        };
        let r = next.ok_or_else(|| format!("{path}: {} has no sound to play", s.class))?;
        eprintln!("{path} ({}) -> {}", s.class, obj.pkg.full_path(r));
        obj = lib.resolve(&obj.pkg, r).ok_or_else(|| format!("cannot resolve {}", obj.pkg.full_path(r)))?;
    }
    Err("sound reference chain too deep".into())
}

fn export(game: &Path, spec: &str, out: &str) -> R<()> {
    let lib = Library::new(game).map_err(|e| e.to_string())?;
    let (path, pcm) = resolve_playable(&lib, find(&lib, game, spec)?)?;
    std::fs::write(out, pcm.to_wav()).map_err(|e| format!("{out}: {e}"))?;
    println!("{path} -> {out} ({}ch {} Hz {:.2}s)", pcm.channels, pcm.sample_rate, pcm.duration_secs());
    Ok(())
}

fn play(game: &Path, spec: &str) -> R<()> {
    let mut sink = rodio::DeviceSinkBuilder::open_default_sink().map_err(|e| format!("audio device: {e}"))?;
    sink.log_on_drop(false);
    let player = rodio::Player::connect_new(sink.mixer());
    let file = [PathBuf::from(spec), game.join(spec), game.join("Music").join(spec)].into_iter().find(|p| p.is_file());
    if let Some(f) = file.filter(|f| f.extension().is_some_and(|e| !e.eq_ignore_ascii_case("uax"))) {
        // Loose music (Ogg Vorbis) or WAV file.
        let data = std::fs::read(&f).map_err(|e| e.to_string())?;
        let dec = rodio::Decoder::try_from(std::io::Cursor::new(data)).map_err(|e| format!("{}: {e}", f.display()))?;
        println!("playing {}", f.display());
        player.append(dec);
    } else {
        let lib = Library::new(game).map_err(|e| e.to_string())?;
        let (path, pcm) = resolve_playable(&lib, find(&lib, game, spec)?)?;
        println!("playing {path} ({}ch {} Hz {:.2}s)", pcm.channels, pcm.sample_rate, pcm.duration_secs());
        let ch = std::num::NonZero::new(pcm.channels).ok_or("zero channels")?;
        let rate = std::num::NonZero::new(pcm.sample_rate).ok_or("zero sample rate")?;
        let samples: Vec<rodio::Sample> = pcm.samples.iter().map(|&s| s as rodio::Sample / 32768.0).collect();
        player.append(rodio::buffer::SamplesBuffer::new(ch, rate, samples));
    }
    player.sleep_until_end();
    Ok(())
}
