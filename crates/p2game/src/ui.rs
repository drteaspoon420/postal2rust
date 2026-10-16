//! Windowed-mode 2D layer: the POSTAL 2 HUD, menus (UWindow shell), console and screens drawn by
//! the original script through the Canvas natives, and player input through p2input.
//!
//! The engine side (interaction graph, key routing, PlayerTick, HUD/menu render passes) is the
//! Bevy-free [`crate::frontend::Frontend`], shared with `--smoke`. This module adapts it to Bevy:
//! keyboard/mouse become UE2 input events, the canvas items it produced go to p2canvas's overlay
//! renderer, and the OS cursor follows script mouse warps.
//!
//! While a menu or screen wants the mouse, the OS cursor drives the UWindow cursor and
//! [`UiInput::captured`] is set so the fly camera stays still.

use crate::frontend::{FrameInput, Frontend};
use crate::window::Sim;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use p2canvas::natives;
use p2canvas::{CanvasPlugin, HudCanvas};
use uscript::vm::ObjId;

/// Input state shared with the fly camera: true while a menu/screen/console owns input.
#[derive(Resource, Default)]
pub(crate) struct UiInput {
    pub(crate) captured: bool,
    /// The game owns the mouse (in-game first-person look); the fly camera should not move.
    pub(crate) game_look: bool,
    /// The 3D view is the player's eye, including while a pause menu or screen is up (the
    /// player's own head and glasses stay hidden then, FIDELITY #20).
    pub(crate) player_view: bool,
}

/// The front end for the running game (non-send: it holds VM-side state).
pub(crate) struct UiState {
    pub(crate) front: Frontend,
    /// `--smoke --windowed`: scripted input and stage checks replace the user's input.
    pub(crate) smoke: Option<crate::smoke::SmokeDriver>,
    /// Stage screenshots still to take: (frames to wait, stage name).
    pub(crate) shots: Vec<(u32, String)>,
}

pub(crate) struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        // `ui_frame` itself is chained into window.rs's frame (after the script tick).
        app.init_resource::<UiInput>();
    }
}

/// Builds the UI layer for a running game: canvas renderer, natives, extension host. Call from
/// `window::run` before the app starts (the first level is already booted).
pub(crate) fn install(app: &mut App, sim: &mut Sim, game_root: &std::path::Path) {
    let lib = sim.game.host.lib.clone();
    let drive_player = !std::env::args().any(|a| a == "--fly");
    let front = Frontend::install(&mut sim.game, game_root, 1600, 900, drive_player);
    let smoke = std::env::args().any(|a| a == "--smoke").then(|| {
        let intro = std::env::args().skip_while(|a| a != "--intro-secs").nth(1).and_then(|s| s.parse().ok()).unwrap_or(120.0);
        crate::smoke::SmokeDriver::new(intro)
    });
    app.add_plugins(CanvasPlugin { lib, clear: None }).insert_non_send(UiState { front, smoke, shots: Vec::new() });
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn ui_frame(
    mut sim: NonSendMut<Sim>,
    mut ui: NonSendMut<UiState>,
    mut hud: ResMut<HudCanvas>,
    mut capture: ResMut<UiInput>,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    mut windows: Query<&mut Window, With<bevy::window::PrimaryWindow>>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    let Ok(mut win) = windows.single_mut() else { return };
    let _t = crate::window::SysTimer::new("ui_frame");
    let (w, h) = (win.physical_width().max(1) as i32, win.physical_height().max(1) as i32);
    let dt = time.delta_secs().clamp(0.0, 0.2);
    let mut events = p2input::bevy_map::collect_events(&keys, &mouse, motion.delta, scroll.delta.y, &[]);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let mut typed: Vec<char> = keys.get_just_pressed().filter_map(|k| keycode_char(*k, shift)).collect();
    let mut mouse_pos = win.cursor_position().map(|p| (p.x, p.y));
    // Scripted test runs (P2GAME_EXEC/HOLD) ignore the OS keyboard and mouse: whatever window
    // has focus on the desktop would otherwise move, fire or even suicide the player. (P2GAME_REAL
    // and P2GAME_PILOT inject their input as OS input and drop the real OS input themselves.)
    let synthetic = std::env::var_os("P2GAME_REAL").is_some() || std::env::var_os("P2GAME_PILOT").is_some();
    if !synthetic && (std::env::var_os("P2GAME_EXEC").is_some() || std::env::var_os("P2GAME_HOLD").is_some()) {
        events.clear();
        typed.clear();
        mouse_pos = None;
    }
    let ui = &mut *ui;
    if let Some(drv) = ui.smoke.as_mut() {
        drv.dt = dt.max(1.0 / 240.0);
        let (ev, m) = drv.input(&mut sim.game, &ui.front);
        events = ev;
        mouse_pos = Some(m);
    }
    let out = ui.front.frame(&mut sim.game, FrameInput { events: &events, typed: &typed, mouse: mouse_pos, width: w, height: h, dt });
    capture.captured = out.captured;
    capture.game_look = out.game_look;
    capture.player_view = out.player_view;
    if let Some(p) = out.mouse_warp {
        win.set_cursor_position(Some(Vec2::new(p.0, p.1)));
    }
    // Matinee `SubActionTitle` overlays (the intro's credit titles): the engine draws camera
    // effects over the rendered frame, after the HUD.
    draw_scene_titles(w, h);
    hud.0.items = natives::with_context(|ctx| std::mem::take(&mut ctx.canvas.items));
    // `P2GAME_ITEMS=frame`: lists the canvas items drawn on that frame.
    if std::env::var("P2GAME_ITEMS").ok().and_then(|s| s.parse::<u32>().ok()) == Some(ui.front.frames) {
        for (i, it) in hud.0.items.iter().enumerate() {
            println!("ITEM {i} {}.{} xywh {:.0},{:.0},{:.0},{:.0} uv {:.2},{:.2}-{:.2},{:.2} color {:?} style {}", it.material.pkg.name, it.material.pkg.name(it.material.pkg.exports[it.material.export].name), it.x, it.y, it.w, it.h, it.u0, it.v0, it.u1, it.v1, it.color.0, it.style);
        }
    }
    if std::env::var_os("P2GAME_UI_TRACE").is_some() && (ui.front.frames < 6 || ui.front.frames % 30 == 0) {
        let vm = &sim.game.world.vm;
        let root = ui.front.root();
        let rs = root.map(|r| {
            let names = ["bWindowVisible", "WinWidth", "WinHeight", "GUIScale", "ActiveWindow", "FirstChildWindow", "LastChildWindow"];
            names.iter().map(|n| format!("{n}={:?}", vm.get_prop(r, n))).collect::<Vec<_>>().join(" ")
        });
        println!("UI frame {} size {w}x{h} items {} menu_up {} root {} {:?}", ui.front.frames, hud.0.items.len(), out.menu_up, root.map(|r| vm.state_name(r)).unwrap_or_default(), rs);
        if let Some(r) = root {
            let mut c = vm.get_prop(r, "FirstChildWindow").and_then(|v| v.as_obj());
            while let Some(o) = c {
                println!("   child {} visible {:?}", vm.object_name(o), vm.get_prop(o, "bWindowVisible"));
                c = vm.get_prop(o, "NextSiblingWindow").and_then(|v| v.as_obj());
            }
            thread_local!(static SEEN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });
            let from = SEEN.with(|s| s.replace(vm.log.len())).min(vm.log.len());
            for l in vm.log.iter().skip(from).filter(|l| !l.contains("Accessed None")) {
                println!("   log {l}");
            }
        }
    }
    if let Some(drv) = ui.smoke.as_mut() {
        // Travel happens later in the frame (window.rs `travel`); a changed level means one happened.
        let traveled = sim.game.level.map.name != *LAST_MAP.with(|m| m.borrow().clone());
        LAST_MAP.with(|m| *m.borrow_mut() = sim.game.level.map.name.clone());
        drv.after(&mut sim.game, &ui.front, traveled, out.menu_up);
        if let Some(st) = drv.finished {
            // Let the stage's frame render (and a new level stream in) before the shot.
            ui.shots.push((if traveled || st == crate::smoke::Stage::Boot { 90 } else { 10 }, st.name().to_string()));
        }
    }
    let out_dir = std::path::PathBuf::from(std::env::args().skip_while(|a| a != "--out").nth(1).unwrap_or_else(|| "out".into())).join("smoke");
    for (n, name) in ui.shots.iter_mut() {
        if *n == 0 {
            let path = out_dir.join(format!("stage_{name}.png"));
            std::fs::create_dir_all(&out_dir).ok();
            println!("smoke: screenshot {}", path.display());
            commands.spawn(bevy::render::view::screenshot::Screenshot::primary_window()).observe(bevy::render::view::screenshot::save_to_disk(path));
        }
        *n = n.wrapping_sub(1);
    }
    ui.shots.retain(|(n, _)| *n != u32::MAX);
    if let Some(drv) = ui.smoke.as_ref() {
        if drv.stage == crate::smoke::Stage::Done && ui.shots.is_empty() {
            DONE_FRAMES.with(|d| d.set(d.get() + 1));
            if DONE_FRAMES.with(|d| d.get()) == 30 {
                let report = drv.report();
                std::fs::write(out_dir.join("smoke_windowed.tsv"), &report).ok();
                println!("\n{report}\nsmoke (windowed): {}", if drv.passed() { "PASSED" } else { "FAILED" });
                exit.write(if drv.passed() { AppExit::Success } else { AppExit::from_code(1) });
            }
        }
    }
    if ui.front.frames == 2 && std::env::var_os("P2GAME_UI_TRACE").is_some() {
        println!("ui: {} canvas items, menu {}, pc {:?}", hud.0.items.len(), out.menu_up, sim.game.player_controller.map(|p| sim.game.world.vm.object_name(p)));
    }
}

thread_local! {
    static LAST_MAP: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    static DONE_FRAMES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}
/// In game, the 3D camera views from the player (`PlayerCalcView`: the pawn's eye at the
/// controller's rotation), so script-driven movement and look are what the player sees.
pub(crate) fn follow_player(mut sim: NonSendMut<Sim>, ui: Res<UiInput>, mut cams: Query<&mut Transform, With<crate::window::FlyCam>>) {
    if !ui.game_look && !ui.player_view {
        return;
    }
    let Some(pc) = sim.game.player_controller else { return };
    // Third-person views (death/suicide cameras, other view targets): the engine asks the
    // controller's `PlayerCalcView` for the camera every frame, so do the same.
    if let Some((loc, r)) = script_view(&mut sim.game.world.vm, pc) {
        let Ok(mut tf) = cams.single_mut() else { return };
        tf.translation = p2view::scene::to_bevy(loc);
        tf.look_to(unreal_forward(r), Vec3::Y);
        return;
    }
    if !ui.game_look {
        return;
    }
    let vm = &sim.game.world.vm;
    let Some(pawn) = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()) else { return };
    if vm.objects[pawn].deleted {
        return;
    }
    let l = vm.get_prop(pawn, "Location").map(|v| v.as_vec()).unwrap_or_default();
    let eye = vm.get_prop(pawn, "EyeHeight").or_else(|| vm.get_prop(pawn, "BaseEyeHeight")).map(|v| v.as_float()).unwrap_or(0.0);
    let r = vm.get_prop(pc, "Rotation").map(|v| v.as_rot()).unwrap_or_default();
    let Ok(mut tf) = cams.single_mut() else { return };
    let (r, e3) = crate::frontend::apply_view_shake(vm, pc, r, eye_position(vm, pawn, l, eye));
    if std::env::var_os("P2GAME_VIEW_TRACE").is_some() {
        println!("VIEW eye {:.1} {:.1} {:.1} eyeheight {eye:.1} walkbob {:?} crouch {:?}/{:?}", e3[0], e3[1], e3[2], vm.get_prop(pawn, "WalkBob").map(|v| v.as_vec()), vm.get_prop(pawn, "bWantsToCrouch").map(|v| v.as_bool()), vm.get_prop(pawn, "bIsCrouched").map(|v| v.as_bool()));
    }
    tf.translation = p2view::scene::to_bevy(e3);
    tf.look_to(unreal_forward(r), Vec3::Y);
}

/// `Pawn.EyePosition` added to the pawn location: `EyeHeight * Z + WalkBob`. The script's
/// `CheckBob` keeps `WalkBob` at zero while the View Bob game option (`P2Player.bWeaponBob`) is off.
pub(crate) fn eye_position(vm: &uscript::vm::Vm, pawn: ObjId, l: [f32; 3], eye: f32) -> [f32; 3] {
    let b = vm.get_prop(pawn, "WalkBob").map(|v| v.as_vec()).unwrap_or_default();
    [l[0] + b[0], l[1] + b[1], l[2] + eye + b[2]]
}

/// Unreal yaw turns from +X towards +Y; pitch up is positive. In Bevy space (Y up, Unreal Y
/// -> Bevy Z) the forward vector is (cos p cos y, sin p, cos p sin y).
fn unreal_forward(r: [i32; 3]) -> Vec3 {
    const U: f32 = std::f32::consts::TAU / 65536.0;
    let (yaw, pitch) = (r[1] as f32 * U, ((r[0] as i16) as f32) * U);
    Vec3::new(pitch.cos() * yaw.cos(), pitch.sin(), pitch.cos() * yaw.sin())
}

/// True when the player is not looking through their own pawn's eyes (`bBehindView`, or a view
/// target other than the pawn).
pub(crate) fn third_person(vm: &uscript::vm::Vm, pc: ObjId) -> bool {
    let pawn = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj());
    let vt = vm.get_prop(pc, "ViewTarget").and_then(|v| v.as_obj());
    let behind = vm.get_prop(pc, "bBehindView").is_some_and(|v| v.as_bool());
    behind || (vt.is_some() && vt != pawn && vt != Some(pc))
}

/// The camera from script `PlayerCalcView` when the view is third person, else None.
fn script_view(vm: &mut uscript::vm::Vm, pc: ObjId) -> Option<([f32; 3], [i32; 3])> {
    if vm.objects[pc].deleted || !third_person(vm, pc) {
        return None;
    }
    let args = vec![uscript::vm::Value::Object(None), uscript::vm::Value::Vector([0.0; 3]), uscript::vm::Value::Rotator([0; 3])];
    match vm.call_event_out(pc, "PlayerCalcView", args) {
        Ok(Some((_, out))) => Some((out.get(1)?.as_vec(), out.get(2)?.as_rot())),
        Ok(None) => None,
        Err(e) => {
            vm.record_error("PlayerCalcView", &e);
            None
        }
    }
}

/// In first person the player's own body parts (head, glasses, body: separate attached actors in
/// POSTAL 2, owned through `MyHead`/`Base` rather than `Owner`) are not drawn, as the engine does
/// for the view target's `bOwnerNoSee` parts.
pub(crate) fn hide_own_body(sim: NonSend<Sim>, ui: Res<UiInput>, rigs: Query<&crate::rigs::VmRig>, mut vis: Query<&mut Visibility, Without<crate::rigs::VmRig>>) {
    if !ui.player_view {
        return;
    }
    let vm = &sim.game.world.vm;
    let Some(pc) = sim.game.player_controller else { return };
    // The behind view shows the Dude's own body.
    if third_person(vm, pc) {
        return;
    }
    // A running Matinee scene owns the camera: the Dude is filmed from outside (intro
    // TrailerScene2 / DudeHeadsOut walks him out of the trailer) while his controller's
    // ViewTarget stays the pawn, so his body must stay visible.
    if p2game::cinema::cinema().is_some_and(|c| c.view.borrow().is_some()) {
        return;
    }
    let Some(pawn) = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()) else { return };
    let mine = |a: ObjId| {
        let mut cur = Some(a);
        for _ in 0..6 {
            let Some(c) = cur else { return false };
            if c == pawn {
                return true;
            }
            cur = vm.get_prop(c, "Base").and_then(|v| v.as_obj()).or_else(|| vm.get_prop(c, "Owner").and_then(|v| v.as_obj()));
        }
        false
    };
    let head = vm.get_prop(pawn, "MyHead").and_then(|v| v.as_obj());
    for r in rigs.iter() {
        if r.actor == pawn || head == Some(r.actor) || mine(r.actor) {
            for &s in &r.sections {
                if let Ok(mut v) = vis.get_mut(s) {
                    *v = Visibility::Hidden;
                }
            }
        }
    }
}


/// Draws the running scene's `CameraTitle`s: the material centred at (`x`,`y`) of the screen,
/// `size_factor` of the screen width wide (aspect kept), in its render style with the title's
/// colour and alpha.
fn draw_scene_titles(w: i32, h: i32) {
    let Some(view) = p2game::cinema::cinema().and_then(|c| c.view.borrow().clone()) else { return };
    if view.titles.is_empty() {
        return;
    }
    natives::with_context(|ctx| {
        for (t, alpha) in &view.titles {
            let Some(mat) = ctx.object(&t.material) else { continue };
            let (mw, mh) = p2canvas::canvas::MaterialSizer::material_size(&*ctx.db, &mat);
            if mw <= 0 || mh <= 0 {
                continue;
            }
            let dw = w as f32 * t.size_factor;
            let dh = dw * mh as f32 / mw as f32;
            let a = (alpha.clamp(0.0, 1.0) * t.color[3] as f32) as u8;
            let cv = &mut ctx.canvas;
            let (style, color) = (cv.style, cv.draw_color);
            cv.style = t.style;
            cv.draw_color = p2canvas::canvas::Rgba([t.color[0], t.color[1], t.color[2], a]);
            cv.set_pos(w as f32 * t.x - dw / 2.0, h as f32 * t.y - dh / 2.0);
            let db = ctx.db.clone();
            ctx.canvas.draw_tile(&*db, Some(&mat), dw, dh, 0.0, 0.0, mw as f32, mh as f32);
            ctx.canvas.style = style;
            ctx.canvas.draw_color = color;
        }
    });
}

/// Printable character for a key press (US layout), for `KeyType`.
fn keycode_char(k: KeyCode, shift: bool) -> Option<char> {
    let c = match k {
        KeyCode::Space => ' ',
        KeyCode::Digit0 => if shift { ')' } else { '0' },
        KeyCode::Digit1 => if shift { '!' } else { '1' },
        KeyCode::Digit2 => if shift { '@' } else { '2' },
        KeyCode::Digit3 => if shift { '#' } else { '3' },
        KeyCode::Digit4 => if shift { '$' } else { '4' },
        KeyCode::Digit5 => if shift { '%' } else { '5' },
        KeyCode::Digit6 => if shift { '^' } else { '6' },
        KeyCode::Digit7 => if shift { '&' } else { '7' },
        KeyCode::Digit8 => if shift { '*' } else { '8' },
        KeyCode::Digit9 => if shift { '(' } else { '9' },
        KeyCode::Minus => if shift { '_' } else { '-' },
        KeyCode::Equal => if shift { '+' } else { '=' },
        KeyCode::Period => if shift { '>' } else { '.' },
        KeyCode::Comma => if shift { '<' } else { ',' },
        KeyCode::Slash => if shift { '?' } else { '/' },
        KeyCode::Semicolon => if shift { ':' } else { ';' },
        KeyCode::Quote => if shift { '"' } else { '\'' },
        _ => {
            let name = format!("{k:?}");
            let l = name.strip_prefix("Key")?;
            let ch = l.chars().next().filter(|_| l.len() == 1)?;
            if shift { ch.to_ascii_uppercase() } else { ch.to_ascii_lowercase() }
        }
    };
    Some(c)
}
