//! p2canvas demo: draws POSTAL 2 HUD textures, animated materials and text in the game's own
//! fonts through the UE2 Canvas API, as HUD script would.
//!
//! ```text
//! p2canvas [--game <dir>] [--time <seconds>] [--screenshot out.png] [--size 1280x960]
//! ```

use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use p2canvas::{CanvasAssets, CanvasPlugin, CanvasStats, HudCanvas, MaterialSizer, Rgba, style};
use std::path::PathBuf;
use std::sync::Arc;
use upkg::library::Library;

const DEFAULT_GAME: &str = r"G:\SteamLibrary\steamapps\common\POSTAL2Complete";

#[derive(Resource, Clone)]
struct Args {
    game: PathBuf,
    time: Option<f32>,
    screenshot: Option<PathBuf>,
    size: (u32, u32),
}

fn parse_args() -> Args {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    Args {
        game: get("--game").map(PathBuf::from).or_else(|| std::env::var_os("POSTAL2_DIR").map(PathBuf::from)).unwrap_or_else(|| DEFAULT_GAME.into()),
        time: get("--time").and_then(|s| s.parse().ok()),
        screenshot: get("--screenshot").map(PathBuf::from),
        size: get("--size").and_then(|s| s.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))).unwrap_or((1280, 960)),
    }
}

fn main() {
    let args = parse_args();
    let lib = Arc::new(Library::new(&args.game).unwrap_or_else(|e| panic!("cannot index game install at {}: {e}", args.game.display())));
    let capture = args.screenshot.is_some();
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "p2canvas - UE2 Canvas demo".into(),
                resolution: bevy::window::WindowResolution::new(args.size.0, args.size.1).with_scale_factor_override(1.0),
                present_mode: if capture { bevy::window::PresentMode::AutoNoVsync } else { bevy::window::PresentMode::AutoVsync },
                ..default()
            }),
            ..default()
        }))
        .add_plugins(CanvasPlugin { lib, clear: Some(Color::srgb(0.16, 0.17, 0.2)) })
        .insert_resource(args)
        .init_resource::<Frames>()
        .add_systems(Update, (draw_hud, capture_system))
        .run();
}

/// What a HUD script would do every frame in `PostRender(Canvas)`.
fn draw_hud(args: Res<Args>, assets: Res<CanvasAssets>, mut hud: ResMut<HudCanvas>, frames: Res<Frames>) {
    let c = &mut hud.0;
    if let Some(t) = args.time {
        c.time = t;
    }
    let db = assets.db.clone();
    let sz: &dyn MaterialSizer = &*db;
    let (w, h) = (c.size_x as f32, c.size_y as f32);
    let mat = |n: &str| {
        let m = assets.material(n);
        if m.is_none() && frames.0 == 1 {
            eprintln!("missing material {n}");
        }
        m
    };
    let font = |n: &str| {
        let f = assets.font(n);
        if f.is_none() && frames.0 == 1 {
            eprintln!("missing font {n}");
        }
        f
    };

    // --- Title, centred, in the fancy font.
    c.font = font("p2fonts.Fancy48");
    c.style = style::NORMAL;
    c.b_center = true;
    c.set_draw_color(255, 255, 255, 255);
    c.set_pos(0.0, 16.0);
    c.draw_text(sz, "POSTAL 2 Canvas", true);
    c.b_center = false;

    // --- Font specimens.
    let mut y = 90.0;
    for (name, color) in [
        ("p2fonts.Plain30", Rgba::new(255, 255, 255, 255)),
        ("p2fonts.Fancy30", Rgba::new(255, 210, 60, 255)),
        ("p2fonts.Plain19", Rgba::new(200, 255, 200, 255)),
        ("p2fonts.Fancy19", Rgba::new(255, 140, 140, 255)),
        ("p2fonts.Plain12", Rgba::new(220, 220, 220, 255)),
        ("p2smallfonts.Bold12", Rgba::new(160, 200, 255, 255)),
        ("engine.SmallFont", Rgba::new(255, 255, 255, 255)),
        ("uwindowfonts.TahomaB20", Rgba::new(255, 255, 160, 255)),
    ] {
        c.font = font(name);
        c.draw_color = color;
        c.set_pos(24.0, y);
        let text = format!("{name}: The quick brown fox jumps over the lazy dog 0123456789");
        c.draw_text(sz, &text, false);
        let (_, th) = c.text_size(&text);
        y += th + 6.0;
    }

    // --- Word-wrapped paragraph in a clipped box (DrawText wraps against ClipX).
    c.font = font("p2fonts.Plain15");
    c.draw_color = Rgba::new(255, 255, 255, 255);
    let (bx, by, bw) = (24.0, y + 10.0, 520.0);
    c.style = style::ALPHA;
    c.draw_color = Rgba::new(0, 0, 0, 150);
    c.set_pos(bx - 8.0, by - 6.0);
    c.draw_rect(sz, mat("engine.WhiteSquareTexture").as_ref(), bw + 16.0, 150.0);
    c.style = style::NORMAL;
    c.draw_color = Rgba::new(255, 255, 255, 255);
    c.set_origin(bx, by);
    c.set_clip(bw, h);
    c.set_pos(0.0, 0.0);
    c.draw_text(sz, "Regarding the paragraph you are reading: DrawText word-wraps against ClipX exactly like the original Canvas, moving CurY down a line height after every line and returning CurX to the start of the row. Sign my petition!", true);
    let (sl_w, sl_h) = c.str_len("Regarding the paragraph you are reading");
    c.set_origin(0.0, 0.0);
    c.set_clip(w, h);

    // --- DrawTextClipped with a hot key and a hard clip edge.
    c.font = font("p2fonts.Plain19");
    c.draw_color = Rgba::new(255, 255, 0, 255);
    c.set_pos(24.0, by + 160.0);
    c.set_clip(560.0, h);
    c.draw_text_clipped(sz, &format!("&Hotkey underline / StrLen={sl_w}x{sl_h} / clipped at ClipX=560 --> cut off text"), true);
    c.set_clip(w, h);

    // --- HUD icons (Postal2Game.P2HUD defaults).
    let icons = [
        "nathans.Inventory.heartinv",
        "HUDPack.Icons.icon_inv_badge",
        "HUDPack.Icons.Icon_Weapon_Shotgun",
        "HUDPack.Icons.Icon_Weapon_Pistol",
        "HUDPack.Icons.Icon_Weapon_Machinegun",
        "HUDPack.Icons.Icon_Inv_Pizza",
        "HUDPack.Icons.Icon_Inv_Doughnut",
        "HUDPack.Icons.Icon_Inv_Cat",
    ];
    c.style = style::MASKED;
    c.draw_color = Rgba::new(255, 255, 255, 255);
    let mut x = 24.0;
    let iy = h - 130.0;
    for n in icons {
        c.set_pos(x, iy);
        // DrawIcon-style: the whole texture, scaled into a 96x96 cell.
        if let Some(m) = mat(n) {
            let (us, vs) = db.size(&m);
            c.draw_tile(sz, Some(&m), 96.0, 96.0, 0.0, 0.0, us as f32, vs as f32);
        }
        x += 104.0;
    }

    // --- Health readout over a bloodsplat (FinalBlend AlphaBlend), like the kill counter.
    c.style = style::NORMAL;
    c.set_pos(w - 330.0, h - 140.0);
    c.draw_color = Rgba::new(255, 255, 255, 255);
    c.draw_rect(sz, mat("nathans.Skins.bloodsplatblend1").as_ref(), 120.0, 120.0);
    c.font = font("p2fonts.Fancy38");
    c.draw_color = Rgba::new(255, 255, 255, 255);
    c.set_pos(w - 312.0, h - 102.0);
    c.draw_text(sz, "100%", false);

    // --- Animated materials: panner (loading progress bar), rotators, oscillator.
    let anims = [
        ("p2misc.Load.ProgressBarFinal", 140.0, 140.0, "TexPanner"),
        ("zo_napalm.General.zo_whirlpool_part1", 140.0, 140.0, "TexRotator"),
        ("pb_textures.Misc.FanBladesRotator", 140.0, 140.0, "TexRotator"),
        ("zo_smeg.Cube_Maps.zo_rainbow_oscillator", 140.0, 140.0, "TexOscillator"),
        ("hospital-sw.othur.throbbinmeat-sw", 140.0, 140.0, "TexOscillator"),
        ("reopeningjune2016tex.Matrix.MatrixCode01ACom", 140.0, 140.0, "Combiner"),
    ];
    let (gx, gy) = (w - 620.0, 345.0);
    c.font = font("p2fonts.Plain10");
    for (i, (n, aw, ah, label)) in anims.into_iter().enumerate() {
        let Some(m) = mat(n) else { continue };
        let (us, vs) = db.size(&m);
        c.style = style::NORMAL;
        c.draw_color = Rgba::new(255, 255, 255, 255);
        let (ax, ay) = (gx + (i % 4) as f32 * (aw + 12.0), gy + (i / 4) as f32 * (ah + 26.0));
        c.set_pos(ax, ay);
        c.draw_tile(sz, Some(&m), aw, ah, 0.0, 0.0, us as f32, vs as f32);
        c.draw_color = Rgba::new(200, 200, 200, 255);
        c.set_pos(ax, ay + ah + 2.0);
        c.draw_text(sz, label, false);
    }

    // --- Stats line.
    c.font = font("engine.SmallFont");
    c.draw_color = Rgba::new(140, 255, 140, 255);
    c.set_pos(w - 330.0, 20.0);
    c.draw_text(sz, &format!("canvas t={:.2}s  quads queued={}", c.time, c.items.len()), false);
}

#[derive(Resource, Default)]
struct Frames(u32);

fn capture_system(mut commands: Commands, args: Res<Args>, mut frames: ResMut<Frames>, stats: Res<CanvasStats>, mut exit: MessageWriter<AppExit>) {
    frames.0 += 1;
    if frames.0 == 5 {
        println!("CANVAS batches={} quads={} materials={} images={}", stats.batches, stats.quads, stats.materials, stats.images);
    }
    let Some(path) = &args.screenshot else { return };
    const WARMUP: u32 = 60;
    if frames.0 == WARMUP {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path.clone()));
    }
    if frames.0 > WARMUP + 10 && (path.exists() || frames.0 > WARMUP + 600) {
        exit.write(AppExit::Success);
    }
}
