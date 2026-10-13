//! Decodes every sound-family object in the given directories (.uax packages, maps, anything that
//! opens as a package) and reports counts by container/codec plus failures. Directories that hold
//! loose .ogg/.wav/.mp3 files (e.g. Music) are inventoried by format too.
//! Run: cargo run --release -p upkg --example sound_scan -- <dir>...

use std::collections::BTreeMap;
use upkg::Package;
use upkg::sound::{Container, codec, decode_pcm, detect_container, parse_wav, read_sound, sound_exports};

#[derive(Default)]
struct Stats {
    objects: BTreeMap<String, usize>,
    codecs: BTreeMap<String, usize>,
    file_types: BTreeMap<String, usize>,
    ok: usize,
    bad: BTreeMap<String, (usize, String)>,
    pcm_seconds: f64,
    raw_bytes: usize,
    rates: BTreeMap<u32, usize>,
    looped: usize,
    other_sound_classes: BTreeMap<String, usize>,
}

fn main() {
    let dirs: Vec<String> = std::env::args().skip(1).collect();
    let mut st = Stats::default();
    let mut loose: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut loose_info: Vec<String> = Vec::new();
    let mut packages = 0usize;
    let t0 = std::time::Instant::now();
    for d in &dirs {
        let mut entries: Vec<_> = std::fs::read_dir(d).unwrap().flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if matches!(ext.as_str(), "ogg" | "wav" | "mp3") {
                let data = std::fs::read(&path).unwrap();
                let ent = loose.entry(format!("{ext} ({:?})", detect_container(&data))).or_default();
                ent.0 += 1;
                ent.1 += data.len();
                if let Some(info) = ogg_info(&data) {
                    loose_info.push(format!("  {:<28} {info}", path.file_name().unwrap().to_string_lossy()));
                }
                continue;
            }
            let Ok(pkg) = Package::open(&path) else { continue };
            packages += 1;
            // Note any sound-like classes we do not handle, to discover subclasses.
            for i in 0..pkg.exports.len() {
                let c = pkg.export_class(i);
                let cl = c.to_ascii_lowercase();
                if cl.contains("sound") && !upkg::sound::SoundKind::is_sound_class(c) {
                    *st.other_sound_classes.entry(c.to_string()).or_default() += 1;
                }
            }
            for i in sound_exports(&pkg) {
                scan_one(&pkg, i, &mut st);
            }
        }
    }
    println!("packages opened: {packages}");
    for (k, n) in &st.objects {
        println!("objects {n:7} {k}");
    }
    for (k, n) in &st.file_types {
        println!("filetype {n:6} {k}");
    }
    for (k, n) in &st.codecs {
        println!("codec  {n:7} {k}");
    }
    for (k, n) in &st.rates {
        println!("rate   {n:7} {k} Hz");
    }
    println!("with loop points: {}", st.looped);
    for (k, n) in &st.other_sound_classes {
        println!("other sound-like class {n:6} {k}");
    }
    for (k, (n, ex)) in &st.bad {
        println!("FAILED {n:7} {k}  e.g. {ex}");
    }
    for (k, (n, b)) in &loose {
        println!("loose file {n:4} {k}  {:.1} MiB", *b as f64 / 1048576.0);
    }
    for l in &loose_info {
        println!("{l}");
    }
    let bad: usize = st.bad.values().map(|v| v.0).sum();
    println!(
        "SOUND SCAN: decoded={} failed={bad} embedded={:.1} MiB audio={:.1} min in {:.2}s",
        st.ok,
        st.raw_bytes as f64 / 1048576.0,
        st.pcm_seconds / 60.0,
        t0.elapsed().as_secs_f64()
    );
}

fn scan_one(pkg: &Package, i: usize, st: &mut Stats) {
    let s = match read_sound(pkg, i) {
        Ok(s) => s,
        Err(e) => {
            fail(st, "read", format!("{}.{}: {e}", pkg.name, pkg.name(pkg.exports[i].name)));
            return;
        }
    };
    *st.objects.entry(s.class.clone()).or_default() += 1;
    if s.data.is_empty() {
        return; // ProceduralSound / SoundGroup: data only.
    }
    *st.file_types.entry(s.file_type.clone()).or_default() += 1;
    st.raw_bytes += s.data.len();
    let key = match s.container() {
        Container::Wav => match parse_wav(s.data) {
            Ok(w) => format!("WAV {} {}-bit {}ch", codec::name(w.format_tag), w.bits_per_sample, w.channels),
            Err(e) => format!("WAV unparsable ({e})"),
        },
        c => format!("{c:?}"),
    };
    *st.codecs.entry(key.clone()).or_default() += 1;
    match decode_pcm(s.data) {
        Ok(p) => {
            st.ok += 1;
            st.pcm_seconds += p.duration_secs();
            *st.rates.entry(p.sample_rate).or_default() += 1;
            if p.loop_frames.is_some() {
                st.looped += 1;
            }
        }
        Err(e) => fail(st, &key, format!("{}.{}: {e}", pkg.name, s.name)),
    }
}

fn fail(st: &mut Stats, key: &str, ex: String) {
    let ent = st.bad.entry(key.to_string()).or_insert((0, String::new()));
    ent.0 += 1;
    if ent.1.is_empty() {
        ent.1 = ex;
    }
}

/// Channels, rate and duration of an Ogg Vorbis file, from the identification header and the last page's granule position.
fn ogg_info(d: &[u8]) -> Option<String> {
    if !d.starts_with(b"OggS") {
        return None;
    }
    let segs = d[26] as usize;
    let body = 27 + segs;
    let id = d.get(body..body + 30)?;
    if &id[1..7] != b"vorbis" {
        return Some("ogg (non-vorbis)".into());
    }
    let ch = id[11];
    let rate = u32::from_le_bytes(id[12..16].try_into().ok()?);
    let last = d.windows(4).rposition(|w| w == b"OggS")?;
    let gran = u64::from_le_bytes(d.get(last + 6..last + 14)?.try_into().ok()?);
    Some(format!("vorbis {ch}ch {rate} Hz {:.1}s", gran as f64 / rate.max(1) as f64))
}
