//! The engine-side front end, without Bevy: the viewport / interaction master / shell menu
//! (p2canvas `MenuHost`), key events through the interactions and then the User.ini bindings into
//! the PlayerController, `PlayerTick`, and the HUD and interaction render passes through the
//! script Canvas.
//!
//! The windowed mode (ui.rs) feeds it Bevy input and draws the canvas items it leaves in the
//! p2canvas context. `--smoke` (smoke.rs) feeds it scripted input headlessly. Both run the same
//! frame code.
//!
//! Per frame ([`Frontend::frame`]):
//! 1. On a new level: the interaction graph is (re)built, the PlayerController is linked to the
//!    viewport, its screens are set up (`SetupScreens`), `InitInputSystem` runs if needed, and
//!    `TravelPostAccept` is sent once per (level, player). The engine does this when the local
//!    player enters a level; without it P2Player waits in PlayerPrepSave with no GameState.
//! 2. Startup map: the shell's main menu is shown on the first rendered frame.
//! 3. Input: the interaction chain (`Process_KeyEvent`: console, then the viewport's local
//!    interactions) sees each event first. Unconsumed events run the key's binding into the
//!    PlayerController. Then `PlayerTick` (PlayerInput + PlayerMove) runs.
//! 4. Render: interactions' PreRender/PostRender with `myHUD.PostRender` slotted underneath, and
//!    the UWindow cursor.

use p2canvas::natives::{self, CanvasContext, MenuClasses, MenuHost};
use p2canvas::shell::ShellNatives;
use p2game::boot::Game;
use p2game::host::GameHost;
use p2input::bevy_map::InputEvent;
use p2input::{Bindings, Input, InputTarget, Key};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use uscript::host::{Host, NativeCall};
use uscript::vm::{ObjId, Value, Vm};

thread_local! {
    /// The canvas extension host's shared state (level hooks are plain fns).
    static SHELL: RefCell<Option<Rc<ShellNatives>>> = const { RefCell::new(None) };
    static MATERIALS: RefCell<Option<Arc<p2canvas::eval::MaterialDb>>> = const { RefCell::new(None) };
    /// Copy of the Input taken while the real one is borrowed by binding execution.
    static SNAPSHOT: RefCell<Option<Input>> = const { RefCell::new(None) };
    /// (level, player) pairs that already got TravelPostAccept.
    static TRAVEL_ACCEPTED: RefCell<Vec<(usize, ObjId)>> = const { RefCell::new(Vec::new()) };
    /// The local PlayerController, for console lines typed into the script Console.
    static CONSOLE_PC: std::cell::Cell<Option<ObjId>> = const { std::cell::Cell::new(None) };
}

/// The engine adds the controller's view shake to the first-person view (script only integrates
/// it in `ViewShake`): `ShakeRot` on the rotation and `ShakeOffset` (view space: X forward,
/// Y right, Z up) on the eye.
pub(crate) fn apply_view_shake(vm: &Vm, pc: ObjId, rot: [i32; 3], eye: [f32; 3]) -> ([i32; 3], [f32; 3]) {
    let sr = vm.get_prop(pc, "ShakeRot").map(|v| v.as_rot()).unwrap_or_default();
    let so = vm.get_prop(pc, "ShakeOffset").map(|v| v.as_vec()).unwrap_or_default();
    let rot = [rot[0].wrapping_add(sr[0]), rot[1].wrapping_add(sr[1]), rot[2].wrapping_add(sr[2])];
    if so == [0.0; 3] {
        return (rot, eye);
    }
    let m = p2anim::rig::rotator_mat(rot);
    let (x, y, z) = (m.x_axis.truncate(), m.y_axis.truncate(), m.z_axis.truncate());
    let d = x * so[0] + y * so[1] + z * so[2];
    (rot, [eye[0] + d.x, eye[1] + d.y, eye[2] + d.z])
}

/// Extension host: canvas resource properties (Texture.USize, Font, ...), offline Steam/travel
/// natives, and console replies for input queries.
struct CanvasExt {
    db: Arc<p2canvas::eval::MaterialDb>,
    shell: Rc<ShellNatives>,
}

impl Host for CanvasExt {
    fn native(&self, vm: &mut Vm, call: &NativeCall, args: &mut Vec<Value>) -> Option<uscript::Result<Value>> {
        // ClientTravel goes through the game host's travel request.
        if call.name.eq_ignore_ascii_case("ClientTravel") {
            // bItems: whether the player's inventory travels (travel.rs).
            let cmd = p2game::travel::client_travel(args);
            let h = vm.host();
            h.console_command(vm, call.this, &cmd);
            return Some(Ok(Value::Void));
        }
        self.shell.native(vm, call, args)
    }

    /// Typed console lines (`Console.ConsoleCommand` from an Interaction): the engine's viewport
    /// Exec runs exec functions on the local PlayerController chain (cheats, Summon, SwitchWeapon,
    /// ...). Engine commands the host owns (open, SaveGame, GET/SET ...) are answered elsewhere and
    /// never reach this ext (the game host consults its ext list only for unhandled commands).
    fn console_command(&self, vm: &mut Vm, from: Option<ObjId>, command: &str) -> String {
        let Some(from) = from else { return String::new() };
        if !vm.class_is_a(vm.objects[from].class, "Interactions") {
            return String::new();
        }
        let Some(pc) = CONSOLE_PC.with(|p| p.get()).filter(|&pc| pc < vm.objects.len() && !vm.objects[pc].deleted) else { return String::new() };
        let verb = command.split_whitespace().next().unwrap_or("");
        if verb.is_empty() || !exec_target_exists(vm, pc, verb) {
            return String::new();
        }
        if exec_command(vm, pc, command) { "1".into() } else { String::new() }
    }

    fn resource_prop(&self, _vm: &mut Vm, path: &str, prop: &str) -> Option<Value> {
        natives::resource_prop(&self.db, path, prop)
    }
}

/// True once the local player of this level got TravelPostAccept. The engine sends it at login,
/// before the level's first tick; the world tick waits for it so start-of-level script that
/// unpossesses the player (AW intro cinematics) does not run first (FIDELITY #54).
pub(crate) fn travel_accepted(level_serial: u64, pc: Option<ObjId>) -> bool {
    pc.is_none_or(|pc| TRAVEL_ACCEPTED.with(|t| t.borrow().contains(&(level_serial as usize, pc))))
}

fn level_hook(host: &GameHost, _vm: &mut Vm) {
    let shell = SHELL.with(|s| s.borrow().clone());
    let db = MATERIALS.with(|m| m.borrow().clone());
    if let (Some(shell), Some(db)) = (shell, db) {
        host.ext.borrow_mut().push(Rc::new(CanvasExt { db, shell }));
    }
}

/// One frame of input for [`Frontend::frame`].
pub(crate) struct FrameInput<'a> {
    pub events: &'a [InputEvent],
    /// Characters typed this frame (console / edit boxes).
    pub typed: &'a [char],
    /// Cursor position in window pixels.
    pub mouse: Option<(f32, f32)>,
    pub width: i32,
    pub height: i32,
    pub dt: f32,
}

#[derive(Default, Debug, Clone, Copy)]
pub(crate) struct FrameOutput {
    /// A menu or screen owns input (the fly camera should stay still).
    pub captured: bool,
    /// In-game first-person look: the camera follows the player.
    pub game_look: bool,
    /// The 3D view is the player's eye (in game, also while a pause menu or screen is up).
    pub player_view: bool,
    /// A script cursor warp the host should apply to the OS cursor.
    pub mouse_warp: Option<(f32, f32)>,
    pub menu_up: bool,
}

pub(crate) struct Frontend {
    pub host: Option<MenuHost>,
    input: Rc<RefCell<Input>>,
    /// Offline platform state (achievements unlocked this session, travel queue).
    #[allow(dead_code)]
    shell: Rc<ShellNatives>,
    /// Level the graph was built for (the map package; object ids are reused after travel).
    level_key: usize,
    /// Frames since the level started (the startup map shows the main menu after first render).
    pub frames: u32,
    /// Script-driven player movement (off: fly camera only).
    pub drive_player: bool,
    menu_shown: bool,
}

impl Frontend {
    /// Installs the canvas natives and the extension host for the running game (and every later
    /// level). Call once, after the first level is booted.
    pub(crate) fn install(game: &mut Game, game_root: &std::path::Path, width: i32, height: i32, drive_player: bool) -> Frontend {
        let lib = game.host.lib.clone();
        let mut ctx = CanvasContext::new(lib, width, height);
        let db = ctx.db.clone();
        let shell = Rc::new(ShellNatives::new(game_root));
        let user = std::fs::read(&game.world.cfg.user_ini_path).unwrap_or_default();
        // Install User.ini gives the defaults, the saved user User.ini overrides keys it lists
        // (rebinds from the controls menu are written there, see `save_rebinds`).
        let base = p2input::bindings::decode_ini(&user);
        let saved = game.world.vm.user_ini_section("User.ini", "Engine.Input");
        let mut lines = p2input::bindings::section_lines(&base, "Engine.Input");
        lines.extend(saved.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let input = Rc::new(RefCell::new(Input::new(Bindings::from_section_lines(lines))));
        let q = input.clone();
        // Script can query input state (ISKEYBIND, BINDING2KEYSTR) from inside an exec function that
        // a key binding is running, while the Input is mutably borrowed; answer from a clone then.
        ctx.console = Some(Box::new(move |c: &str| match q.try_borrow_mut() {
            Ok(mut i) => i.console_query(c),
            Err(_) => SNAPSHOT.with(|s| s.borrow_mut().as_mut().and_then(|i| i.console_query(c))),
        }));
        natives::install(ctx);
        natives::register(&mut game.world.vm);
        SHELL.with(|s| *s.borrow_mut() = Some(shell.clone()));
        MATERIALS.with(|m| *m.borrow_mut() = Some(db));
        p2game::host::register_level_hook(level_hook);
        level_hook(&game.host, &mut game.world.vm);
        Frontend { host: None, input, shell, level_key: 0, frames: 0, drive_player, menu_shown: false }
    }

    /// Writes `set input` changes to the user User.ini `[Engine.Input]` (UE2 saves on each rebind).
    fn save_rebinds(&self, game: &mut Game) {
        let changed = match self.input.try_borrow_mut() {
            Ok(mut i) if !i.changed_bindings.is_empty() => std::mem::take(&mut i.changed_bindings),
            _ => return,
        };
        for (key, value) in changed {
            if let Err(e) = game.world.vm.user_ini_set("User.ini", "Engine.Input", &key, &value) {
                eprintln!("[input] saving binding {key}: {e}");
            }
        }
    }

    /// The shell root window (`ShellRootWindow`), if the interaction graph exists.
    pub(crate) fn root(&self) -> Option<ObjId> {
        self.host.as_ref().and_then(|h| h.base_menu)
    }

    /// One frame: see the module docs.
    pub(crate) fn frame(&mut self, game: &mut Game, inp: FrameInput) -> FrameOutput {
        let mut out = FrameOutput::default();
        let dt = inp.dt;
        let pc = game.player_controller;
        self.save_rebinds(game);

        // (Re)build the interaction graph for this VM level (boot, travel).
        let level_key = game.level_serial as usize;
        if self.host.is_none() || self.level_key != level_key {
            let vm = &mut game.world.vm;
            let cfg = &game.world.cfg;
            let engine = |k: &str| cfg.main.get("Engine.Engine", k).map(str::to_string);
            let classes = MenuClasses { console: engine("Console"), base_menu: engine("BaseMenu"), default_player_menu: None };
            match MenuHost::new(vm, &classes, None) {
                Ok(mut mh) => {
                    if let Some(pc) = pc {
                        mh.attach_player(vm, pc);
                        // P2Player.Possess attaches its screens to the viewport; the engine links the
                        // viewport before PostLogin, so redo that step now that it exists.
                        let c = vm.objects[pc].class;
                        if vm.find_function(c, "SetupScreens").is_some() {
                            if let Err(e) = vm.call(c, Some(pc), "SetupScreens", vec![]) {
                                vm.record_error("SetupScreens", &e);
                            }
                        }
                        if vm.get_prop(pc, "PlayerInput").and_then(|v| v.as_obj()).is_none() {
                            vm.fire_event(pc, "InitInputSystem", vec![]);
                        }
                        // DudePlayer.Possess adds the weapon selector, which then sees the default
                        // inventory (hands, group 0) arrive through NotifyAddInventory. Possess ran
                        // before the viewport existed here, so rebuild it from the inventory now.
                        if vm.find_function(c, "AddWeaponSelector").is_some() && vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()).is_some() {
                            if let Err(e) = vm.call(c, Some(pc), "AddWeaponSelector", vec![]) {
                                vm.record_error("AddWeaponSelector", &e);
                            }
                            if let Some(ws) = vm.get_prop(pc, "WeaponSelector").and_then(|v| v.as_obj()) {
                                let wc = vm.objects[ws].class;
                                if let Err(e) = vm.call(wc, Some(ws), "RefreshSelector", vec![]) {
                                    vm.record_error("RefreshSelector", &e);
                                }
                            }
                        }
                        // The engine calls TravelPostAccept once the local player is in the level
                        // (P2Player: GameState for the day, PostTravel, screens).
                        if !TRAVEL_ACCEPTED.with(|t| t.borrow().contains(&(level_key, pc))) {
                            TRAVEL_ACCEPTED.with(|t| t.borrow_mut().push((level_key, pc)));
                            vm.fire_event(pc, "TravelPostAccept", vec![]);
                        }
                    }
                    self.host = Some(mh);
                }
                Err(e) => {
                    eprintln!("frontend: interaction master: {e}");
                    return out;
                }
            }
            self.level_key = level_key;
            self.frames = 0;
            self.menu_shown = false;
        }
        let Some(mh) = self.host.as_mut() else { return out };
        CONSOLE_PC.with(|p| p.set(pc));
        let vm = &mut game.world.vm;

        // Startup map: show the shell's main menu (the engine does this once the viewport renders).
        let map = game.level.map.name.to_ascii_lowercase();
        let startup = game.world.cfg.startup_map.to_ascii_lowercase();
        let startup = std::path::Path::new(&startup).file_stem().and_then(|s| s.to_str()).unwrap_or("startup").to_string();
        if !self.menu_shown && self.frames == 1 && std::env::var_os("P2GAME_UI_TRACE").is_some() {
            println!("frontend: map {map:?} startup {startup:?} base_menu {:?}", mh.base_menu);
        }
        // The startup map's script first runs the shell's precache pass (ShowMenu with bPrecache,
        // one black-covered paint, then HideMenu); showing the real menu during that pass gets it
        // hidden again, so wait until precache is over (or never started).
        let precaching = mh.base_menu.is_some_and(|r| vm.get_prop(r, "bPrecache").is_some_and(|v| v.as_bool()));
        let precached = mh.base_menu.is_some_and(|r| vm.get_prop(r, "bDidPrecache").is_some_and(|v| v.as_bool()));
        let show_now = self.frames >= 1 && !precaching && (precached || self.frames >= 10);
        if !self.menu_shown && show_now && map == startup && std::env::var_os("P2GAME_NO_AUTOMENU").is_none() {
            if let Some(root) = mh.base_menu {
                let c = vm.objects[root].class;
                if let Err(e) = vm.call(c, Some(root), "ShowMenu", vec![]) {
                    vm.record_error("ShowMenu", &e);
                    if std::env::var_os("P2GAME_UI_TRACE").is_some() {
                        println!("frontend: ShowMenu error {e:?}");
                    }
                }
                if std::env::var_os("P2GAME_UI_TRACE").is_some() {
                    println!("frontend: after ShowMenu root {} state {} visible {:?}", vm.object_name(root), vm.state_name(root), vm.get_prop(root, "bWindowVisible"));
                }
            }
            self.menu_shown = true;
        }

        // Level time that keeps running while paused (menus and screens use it). One step is
        // capped at 50 ms: a first-use texture upload (the map screen's 1024x1024 page) stalls a
        // frame for ~0.2 s and would otherwise skip most of a screen's 0.5 s fade.
        if let Some(l) = vm.level {
            let t = vm.get_prop(l, "TimeSecondsAlways").map(|v| v.as_float()).unwrap_or(0.0) + dt.min(0.05);
            vm.set_prop(l, "TimeSecondsAlways", Value::Float(t));
            if std::env::var_os("P2GAME_SCREEN_TRACE").is_some() {
                if let Some(Value::Array(locals)) = vm.get_prop(mh.player, "LocalInteractions").cloned() {
                    for s in locals.iter().filter_map(|v| v.as_obj()).filter(|&o| vm.class_is_a(vm.objects[o].class, "P2Screen")) {
                        if vm.get_prop(s, "bActive").is_some_and(|v| v.as_bool()) {
                            println!("SCREEN t={t:.3} dt={dt:.3} {} state {} fade {:?} msg {:?}/{:?} delayed {:?}->{:?} at {:?} {:?}", vm.object_name(s), vm.state_name(s), vm.get_prop(s, "FadeAlpha").map(|v| v.as_float()), vm.get_prop(s, "bShowMsg").map(|v| v.as_bool()), vm.get_prop(s, "bMsgFlashOn").map(|v| v.as_bool()), vm.get_prop(s, "bDelayedGoto").map(|v| v.as_bool()), vm.get_prop(s, "DelayedState").map(|v| v.as_name()), vm.get_prop(s, "DelayTime").map(|v| v.as_float()), vm.get_prop(s, "Message").map(|v| v.as_str()));
                        }
                    }
                }
            }
        }

        // Mouse: the UWindow cursor follows the OS cursor.
        let tr = std::env::var_os("P2GAME_UI_TRACE").is_some() && (3..7).contains(&self.frames);
        let phase = |vm: &Vm, mh: &MenuHost, what: &str| {
            if tr {
                if let Some(r) = mh.base_menu {
                    println!("  phase {what}: root state {} visible {:?} precache {:?} did {:?} virgin {:?}", vm.state_name(r), vm.get_prop(r, "bWindowVisible"), vm.get_prop(r, "bPrecache"), vm.get_prop(r, "bDidPrecache"), vm.get_prop(r, "bVirgin"));
                }
            }
        };
        phase(vm, mh, "start");
        let menu_up = mh.menu_active(vm);
        let screen_up = any_screen_running(vm, mh);
        out.menu_up = menu_up;
        out.captured = menu_up || screen_up;
        out.game_look = self.drive_player && pc.is_some() && !out.captured;
        out.player_view = self.drive_player && pc.is_some();
        if let Some(p) = inp.mouse {
            mh.set_mouse(vm, p.0, p.1);
        }
        phase(vm, mh, "mouse");

        // Keys: interactions first, then bindings.
        let mut events = inp.events.to_vec();
        // Test hook: `P2GAME_HOLD=W:30:120` holds a key from frame 30 to 120 (reports pawn movement).
        if let Ok(spec) = std::env::var("P2GAME_HOLD") {
            for one in spec.split(';') {
                hold_hook(one, vm, pc, self.frames, &mut events, game.host.world.borrow().is_some());
            }
        }
        SNAPSHOT.with(|s| *s.borrow_mut() = Some(self.input.borrow().clone()));
        let mut input = self.input.borrow_mut();
        if let Some(pc) = pc {
            if self.drive_player && !out.captured {
                // The PlayerController accumulates raw axes each frame; PlayerInput consumes them.
                for n in ["aBaseX", "aBaseY", "aBaseZ", "aMouseX", "aMouseY", "aForward", "aTurn", "aStrafe", "aUp", "aLookUp"] {
                    vm.set_prop(pc, n, Value::Float(0.0));
                }
            }
        }
        let key_trace = std::env::var_os("P2GAME_KEY_TRACE").is_some();
        for e in events {
            if e.key.is_axis() {
                if out.captured {
                    continue; // menus follow the OS cursor
                }
            } else if !input.pre_process(e.key, e.action) {
                if key_trace {
                    println!("KEY {} {:?}: dropped by pre_process", e.key.name(), e.action);
                }
                continue;
            }
            if mh.key_event(vm, e.key.0, e.action as u8, e.delta) {
                if key_trace && !e.key.is_axis() {
                    println!("KEY {} {:?}: consumed by an interaction", e.key.name(), e.action);
                }
                continue;
            }
            if key_trace && !e.key.is_axis() {
                println!("KEY {} {:?}: to bindings", e.key.name(), e.action);
            }
            if let (Some(pc), true) = (pc, self.drive_player) {
                let mut t = PcTarget { vm, pc };
                input.process(&mut t, e.key, e.action, e.delta);
            }
        }
        // Typed characters for console/edit boxes (KeyType).
        for ch in inp.typed {
            mh.key_type(vm, Key::UNICODE.0, &ch.to_string());
        }
        if let (Some(pc), true) = (pc, self.drive_player) {
            let mut t = PcTarget { vm, pc };
            input.finish_frame(&mut t, dt);
            drop(input);
            // PlayerTick: PlayerInput (axis remap, mouse smoothing) then PlayerMove (state code).
            if !out.captured && !vm.objects[pc].deleted {
                let r0 = vm.get_prop(pc, "Rotation").cloned();
                let pj0 = vm.get_prop(pc, "bPressedJump").is_some_and(|v| v.as_bool());
                let phys0 = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()).and_then(|p| vm.get_prop(p, "Physics")).map(|v| v.as_int());
                vm.fire_event(pc, "PlayerTick", vec![Value::Float(dt)]);
                if std::env::var_os("P2GAME_JUMP_TRACE").is_some() {
                    thread_local!(static LAST: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) });
                    let pawn = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj());
                    let phys = pawn.and_then(|p| vm.get_prop(p, "Physics")).map(|v| v.as_int());
                    let st = format!("{} phys {phys:?}", vm.state_name(pc));
                    if vm.state_name(pc) == "PlayerPrepSave" {
                        let paused = vm.level.and_then(|l| vm.get_prop(l, "Pauser")).and_then(|v| v.as_obj()).is_some();
                        let t = vm.level.and_then(|l| vm.get_prop(l, "TimeSeconds")).map(|v| v.as_float()).unwrap_or(0.0);
                        println!("PREP@{} dt {dt:.3} t={t:.2} paused {paused}", self.frames);
                    }
                    if LAST.with(|l| *l.borrow() != st) {
                        let t = vm.level.and_then(|l| vm.get_prop(l, "TimeSeconds")).map(|v| v.as_float()).unwrap_or(0.0);
                        let l = pawn.map(|p| vm.actor_loc(p)).unwrap_or_default();
                        let v = pawn.and_then(|p| vm.get_prop(p, "Velocity")).map(|v| v.as_vec()).unwrap_or_default();
                        thread_local!(static T0: std::time::Instant = std::time::Instant::now());
                        let wall = T0.with(|t0| t0.elapsed().as_secs_f32());
                        println!("PCSTATE@{} t={t:.2} wall={wall:.2} {st} loc {:.0} {:.0} {:.0} vel {:.0} {:.0} {:.0}", self.frames, l[0], l[1], l[2], v[0], v[1], v[2]);
                        LAST.with(|l| *l.borrow_mut() = st);
                    }
                    // Falling without vertical motion (perched on something that is not a floor):
                    // Jump and door bumps do nothing until P2Player.DetectStuckPlayer warps him.
                    thread_local!(static STALL: std::cell::Cell<u32> = const { std::cell::Cell::new(0) });
                    let vz = pawn.and_then(|p| vm.get_prop(p, "Velocity")).map(|v| v.as_vec()[2]).unwrap_or(0.0);
                    let n = if phys == Some(2) && vz.abs() < 1.0 { STALL.with(|s| s.get() + 1) } else { 0 };
                    STALL.with(|s| s.set(n));
                    if n == 10 || (n > 0 && n % 120 == 0) {
                        let p = pawn.unwrap();
                        let l = vm.actor_loc(p);
                        let under = p2game::physics::current().and_then(|h| {
                            let mut w = h.world.borrow_mut();
                            let id = h.bridge.borrow().phys(p);
                            let hit = w.trace(glam::Vec3::from(l), glam::Vec3::from(l) - glam::Vec3::Z * 120.0, glam::Vec3::new(20.0, 20.0, 1.0), true, id)?;
                            let what = h.bridge.borrow().hit_object(vm, hit.actor).map(|o| vm.object_name(o)).unwrap_or_else(|| format!("{:?}", hit.actor));
                            Some(format!("{what} n {:.2?} dz {:.0}", hit.normal, l[2] - hit.location.z))
                        });
                        println!("STUCKFALL@{} frames {n} loc {:.0} {:.0} {:.0} base {:?} under {under:?}", self.frames, l[0], l[1], l[2], vm.get_prop(p, "Base").and_then(|v| v.as_obj()).map(|b| vm.object_name(b)));
                    }
                    // A jump that ends after one frame: name what is above / overlapping the Dude
                    // (playtest: jumps landed instantly for ~12 s at a time).
                    thread_local!(static JUMPED: std::cell::Cell<Option<f32>> = const { std::cell::Cell::new(None) });
                    if let (Some(p), Some(z0)) = (pawn, JUMPED.with(|j| j.take())) {
                        let l = vm.actor_loc(p);
                        if phys == Some(1) && (l[2] - z0).abs() < 2.0 {
                            let info = p2game::physics::current().map(|h| {
                                let id = h.bridge.borrow().phys(p);
                                let mut w = h.world.borrow_mut();
                                let (r, ht) = id.and_then(|i| w.get(i).map(|a| (a.collision_radius, a.collision_height))).unwrap_or((25.0, 69.0));
                                let at = glam::Vec3::from(l);
                                let sweep = id.and_then(|i| w.sweep(i, at, glam::Vec3::Z * 20.0, &[])).map(|hit| {
                                    let what = h.bridge.borrow().hit_object(vm, hit.actor).map(|o| format!("{} [{}]", vm.object_name(o), vm.state_name(o))).unwrap_or_else(|| format!("{:?}", hit.actor));
                                    format!("{what} t {:.2} n {:.2?} solid {} depth {:.1}", hit.t, hit.normal, hit.start_solid, hit.depth)
                                });
                                let up = w.trace(at, at + glam::Vec3::Z * 40.0, glam::Vec3::new(r, r, ht), true, id).map(|hit| {
                                    let what = h.bridge.borrow().hit_object(vm, hit.actor).map(|o| format!("{} [{}]", vm.object_name(o), vm.state_name(o))).unwrap_or_else(|| format!("{:?}", hit.actor));
                                    format!("{what} t {:.2} n {:.2?}", hit.t, hit.normal)
                                });
                                let near: Vec<String> = w
                                    .colliding_actors(at, r + 60.0)
                                    .into_iter()
                                    .filter(|&o| Some(o) != id)
                                    .filter_map(|o| {
                                        let a = w.get(o)?;
                                        let obj = h.bridge.borrow().obj(o)?;
                                        Some(format!("{} phys {:?} base {:?} block {} r {:.0} h {:.0} dz {:.0}", vm.object_name(obj), a.physics, a.base.and_then(|b| h.bridge.borrow().obj(b)).map(|b| vm.object_name(b)), a.block_actors, a.collision_radius, a.collision_height, a.location.z - at.z))
                                    })
                                    .collect();
                                format!("sweep {sweep:?} up {up:?} near {near:?}")
                            });
                            println!("JUMPCUT@{} loc {:.0} {:.0} {:.0} jumpz {:?} walking {:?} base {:?} {}", self.frames, l[0], l[1], l[2], vm.get_prop(p, "JumpZ"), vm.get_prop(p, "bIsWalking"), vm.get_prop(p, "Base").and_then(|v| v.as_obj()).map(|b| (vm.object_name(b), vm.get_prop(b, "bWorldGeometry").cloned(), vm.get_prop(b, "Velocity").cloned())), info.unwrap_or_default());
                        }
                    }
                    if let Some(p) = pawn.filter(|_| phys == Some(2) && pj0) {
                        JUMPED.with(|j| j.set(Some(vm.actor_loc(p)[2])));
                    }
                }
                // `P2GAME_JUMP_TRACE=1`: the controller/pawn state on every frame a jump was pressed.
                if pj0 && phys0.is_some() && std::env::var_os("P2GAME_JUMP_TRACE").is_some() {
                    let pawn = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj());
                    let g = |n: &str| pawn.and_then(|p| vm.get_prop(p, n)).cloned();
                    println!(
                        "JUMP@{} phys0 {phys0:?} pc {} pawn {:?} phys {:?} vel {:?} crouched {:?} wantcrouch {:?} base {:?} pj after {:?}",
                        self.frames,
                        vm.state_name(pc),
                        pawn.map(|p| vm.state_name(p)),
                        g("Physics"),
                        g("Velocity"),
                        g("bIsCrouched"),
                        g("bWantsToCrouch"),
                        pawn.and_then(|p| vm.get_prop(p, "Base")).and_then(|v| v.as_obj()).map(|b| vm.object_name(b)),
                        vm.get_prop(pc, "bPressedJump"),
                    );
                }
                if std::env::var_os("P2GAME_ROT_TRACE").is_some() && self.frames % 30 == 0 {
                    let in_phys = p2game::physics::current().map(|h| h.bridge.borrow().phys(pc).is_some());
                    println!("ROT@{} before {:?} after {:?} aTurn {:?} pc in physics {in_phys:?} phys {:?}", self.frames, r0, vm.get_prop(pc, "Rotation"), vm.get_prop(pc, "aTurn"), vm.get_prop(pc, "Physics"));
                }
            }
        } else {
            drop(input);
        }

        // Camera for WorldToScreen / DrawActor: the player's view when there is one.
        if let Some(pc) = pc {
            let loc = vm.get_prop(pc, "Location").map(|v| v.as_vec()).unwrap_or_default();
            let rot = vm.get_prop(pc, "Rotation").map(|v| v.as_rot()).unwrap_or_default();
            let fov = vm.get_prop(pc, "FovAngle").map(|v| v.as_float()).filter(|f| *f > 1.0).unwrap_or(85.0);
            let eye = vm
                .get_prop(pc, "Pawn")
                .and_then(|v| v.as_obj())
                .map(|p| {
                    let l = vm.get_prop(p, "Location").map(|v| v.as_vec()).unwrap_or(loc);
                    // The view location (CalcFirstPersonView: Pawn.EyePosition), same as the
                    // world camera, so the weapon's CalcDrawOffset bob is relative to it.
                    let e = vm.get_prop(p, "EyeHeight").or_else(|| vm.get_prop(p, "BaseEyeHeight")).map(|v| v.as_float()).unwrap_or(0.0);
                    crate::ui::eye_position(vm, p, l, e)
                })
                .unwrap_or(loc);
            let (rot, eye) = crate::frontend::apply_view_shake(vm, pc, rot, eye);
            natives::with_context(|ctx| ctx.camera = natives::Camera { location: eye, rotation: rot, fov });
        }

        // Render: interactions' PreRender/PostRender, with the HUD slotted underneath them.
        // Canvas items (and DrawActor requests) stay in the p2canvas context for the host.
        phase(vm, mh, "input");
        mh.frame(vm, dt, inp.width, inp.height);
        phase(vm, mh, "interactions");
        render_hud(vm, mh, pc);
        out.mouse_warp = mh.take_mouse_warp(vm);
        mh.draw_cursor(vm);
        self.frames += 1;
        out
    }
}

/// `P2GAME_HOLD=Key:from:to`: holds a key over a frame range and prints the pawn's movement.
fn hold_hook(spec: &str, vm: &mut Vm, pc: Option<ObjId>, frame: u32, events: &mut Vec<InputEvent>, world_host: bool) {
    let p: Vec<&str> = spec.split(':').collect();
    let (Some(k), Some(a), Some(b)) = (p.first().and_then(|k| Key::from_name(k)), p.get(1).and_then(|s| s.parse::<u32>().ok()), p.get(2).and_then(|s| s.parse::<u32>().ok())) else { return };
    let pawn_loc = |vm: &Vm| pc.and_then(|pc| vm.get_prop(pc, "Pawn")).and_then(|v| v.as_obj()).and_then(|p| vm.get_prop(p, "Location")).map(|v| v.as_vec());
    if frame == a {
        events.push(InputEvent { key: k, action: p2input::Action::Press, delta: 0.0 });
        println!("HOLD {} pressed, pawn at {:?}", k.name(), pawn_loc(vm));
    }
    if frame == b {
        events.push(InputEvent { key: k, action: p2input::Action::Release, delta: 0.0 });
        let pcs = pc.map(|pc| (vm.get_prop(pc, "aBaseY").cloned(), vm.get_prop(pc, "aForward").cloned(), vm.state_name(pc)));
        println!("HOLD {} released, pawn at {:?} pc {:?}", k.name(), pawn_loc(vm), pcs);
        if let Some(pc) = pc {
            let pi = vm.get_prop(pc, "PlayerInput").cloned();
            let pawn = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj());
            let ph = pawn.map(|p| (vm.get_prop(p, "Physics").cloned(), vm.get_prop(p, "Acceleration").cloned(), vm.get_prop(p, "Velocity").cloned()));
            println!("  PlayerInput={pi:?} pawn physics/accel/vel={ph:?} world host={world_host}");
        }
    }
}

/// The PlayerController as an input target.
struct PcTarget<'a> {
    vm: &'a mut Vm,
    pc: ObjId,
}

impl InputTarget for PcTarget<'_> {
    fn input_byte(&mut self, name: &str) -> Option<u8> {
        self.vm.get_prop(self.pc, name).map(|v| v.as_int() as u8)
    }
    fn set_input_byte(&mut self, name: &str, v: u8) {
        self.vm.set_prop(self.pc, name, Value::Byte(v));
    }
    fn input_float(&mut self, name: &str) -> Option<f32> {
        self.vm.get_prop(self.pc, name).map(|v| v.as_float())
    }
    fn set_input_float(&mut self, name: &str, v: f32) {
        self.vm.set_prop(self.pc, name, Value::Float(v));
    }
    fn exec(&mut self, command: &str) -> bool {
        // `P2GAME_JUMP_TRACE=1`: the foot weapon's state around every kick (a stuck NormalFire
        // blocks later kicks).
        let verb = command.split_whitespace().next().unwrap_or("");
        let kick = (verb.eq_ignore_ascii_case("DoKick") || verb.eq_ignore_ascii_case("Jump")) && std::env::var_os("P2GAME_JUMP_TRACE").is_some();
        let foot_state = |vm: &Vm, pc: ObjId| {
            let pawn = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj());
            let pl = pawn.map(|p| vm.actor_loc(p)).unwrap_or_default();
            let dist = |a: ObjId| {
                let l = vm.actor_loc(a);
                ((l[0] - pl[0]).powi(2) + (l[1] - pl[1]).powi(2) + (l[2] - pl[2]).powi(2)).sqrt()
            };
            let door = vm
                .actors
                .iter()
                .copied()
                .filter(|&a| !vm.objects[a].deleted && vm.get_prop(a, "KeyNum").is_some() && vm.get_prop(a, "CurrentStuckCount").is_some())
                .min_by(|&a, &b| dist(a).total_cmp(&dist(b)));
            let door = door.map(|d| {
                let g = |n: &str| vm.get_prop(d, n).map(|v| format!("{v:?}")).unwrap_or_default();
                format!("{} {} d {:.0} key {} prev {} open {} stuck {} health {}", vm.object_name(d), vm.state_name(d), dist(d), g("KeyNum"), g("PrevKeyNum"), g("bOpening"), g("CurrentStuckCount"), g("Health"))
            });
            let foot = pawn.and_then(|p| vm.get_prop(p, "MyFoot")).and_then(|v| v.as_obj());
            let wep = pawn.and_then(|p| vm.get_prop(p, "Weapon")).and_then(|v| v.as_obj());
            format!(
                "foot {:?} {:?} weapon {:?} {:?} pawn {:?} phys {:?} door {door:?}",
                foot.map(|f| vm.object_name(f)),
                foot.map(|f| vm.state_name(f)),
                wep.map(|w| vm.object_name(w)),
                wep.map(|w| vm.state_name(w)),
                pawn.map(|p| vm.state_name(p)),
                pawn.and_then(|p| vm.get_prop(p, "Physics")).map(|v| v.as_int()),
            )
        };
        let before = kick.then(|| foot_state(self.vm, self.pc));
        let ok = exec_command(self.vm, self.pc, command);
        if let Some(b) = before {
            let paused = self.vm.level.and_then(|l| self.vm.get_prop(l, "Pauser")).and_then(|v| v.as_obj()).is_some();
            let pj = self.vm.get_prop(self.pc, "bPressedJump").is_some_and(|v| v.as_bool());
            println!("EXEC {verb} pc {} paused {paused} pressedjump {pj} before [{b}] after [{}]", self.vm.state_name(self.pc), foot_state(self.vm, self.pc));
        }
        if std::env::var_os("P2GAME_KEY_TRACE").is_some() {
            println!("KEY exec {command:?} -> {ok}");
        }
        ok
    }
}

/// Objects consulted for exec functions, in order: controller, cheat manager, pawn, HUD, weapon,
/// GameInfo.
fn exec_targets(vm: &mut Vm, pc: ObjId) -> Vec<ObjId> {
    ensure_cheat_manager(vm, pc);
    let mut targets = vec![Some(pc), vm.get_prop(pc, "CheatManager").and_then(|v| v.as_obj())];
    for p in ["Pawn", "myHUD"] {
        targets.push(vm.get_prop(pc, p).and_then(|v| v.as_obj()));
    }
    if let Some(pawn) = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()) {
        targets.push(vm.get_prop(pawn, "Weapon").and_then(|v| v.as_obj()));
    }
    if let Some(l) = vm.level {
        targets.push(vm.get_prop(l, "Game").and_then(|v| v.as_obj()));
    }
    targets.into_iter().flatten().filter(|&t| t < vm.objects.len() && !vm.objects[t].deleted).collect()
}

/// True if some exec target has a function called `f`.
fn exec_target_exists(vm: &mut Vm, pc: ObjId, f: &str) -> bool {
    exec_targets(vm, pc).into_iter().any(|t| vm.find_function(vm.objects[t].class, f).is_some())
}

/// Runs an exec command the way the engine's console does: an exec function on the
/// PlayerController, its CheatManager, pawn/weapon/HUD/GameInfo, or a host console command.
pub(crate) fn exec_command(vm: &mut Vm, pc: ObjId, command: &str) -> bool {
    let mut it = command.splitn(2, char::is_whitespace);
    let Some(f) = it.next().filter(|f| !f.is_empty()) else { return false };
    let rest = it.next().unwrap_or("");
    for t in exec_targets(vm, pc) {
        let c = vm.objects[t].class;
        if vm.find_function(c, f).is_some() {
            // Words convert to the exec function's parameter types (SwitchWeapon takes a byte).
            let args = vm.exec_args(t, f, rest).unwrap_or_default();
            if let Err(e) = vm.call_event(t, f, args) {
                vm.record_error(&format!("exec {f}"), &e);
            }
            return true;
        }
    }
    // Not an exec function: the engine console chain (UInput::Exec for `set input`/key queries,
    // then the host's console commands).
    let reply = natives::console_command(vm, Some(pc), command);
    if std::env::var_os("P2GAME_EXEC").is_some() {
        if let Some(r) = reply.filter(|r| !r.is_empty()) {
            println!("   console reply: {r}");
        }
    }
    false
}

/// `PlayerController.CheatManager` from `CheatClass` when script has not made one yet (the engine
/// creates it with the controller as Outer; P2CheatManager is `within PlayerController`).
fn ensure_cheat_manager(vm: &mut Vm, pc: ObjId) {
    if vm.get_prop(pc, "CheatManager").and_then(|v| v.as_obj()).is_some() {
        return;
    }
    let Some(Value::Class(Some(c))) = vm.get_prop(pc, "CheatClass").cloned() else { return };
    let cm = vm.new_object(c);
    vm.set_prop(cm, "Outer", Value::Object(Some(pc)));
    vm.set_prop(pc, "CheatManager", Value::Object(Some(cm)));
}

/// True if a P2Screen (load/map/news screen) is running on the viewport.
pub(crate) fn any_screen_running(vm: &Vm, mh: &MenuHost) -> bool {
    let Some(Value::Array(locals)) = vm.get_prop(mh.player, "LocalInteractions").cloned() else { return false };
    locals.iter().filter_map(|v| v.as_obj()).any(|o| {
        vm.class_is_a(vm.objects[o].class, "P2Screen") && vm.get_prop(o, "bActive").is_some_and(|v| v.as_bool()) && vm.get_prop(o, "bVisible").is_some_and(|v| v.as_bool())
    })
}

/// `myHUD.PostRender(Canvas)` with the frame's script Canvas (as `UGameEngine::Draw` does after
/// the world render, before the interactions' PostRender).
fn render_hud(vm: &mut Vm, mh: &MenuHost, pc: Option<ObjId>) {
    let Some(pc) = pc else { return };
    let Some(hud) = vm.get_prop(pc, "myHUD").and_then(|v| v.as_obj()) else { return };
    if vm.objects[hud].deleted {
        return;
    }
    let c = Value::Object(Some(mh.canvas));
    // Draw the HUD under everything already queued by interactions this frame.
    let before = natives::with_context(|ctx| std::mem::take(&mut ctx.canvas.items));
    mh.reset_canvas(vm, natives::with_context(|ctx| ctx.canvas.size_x), natives::with_context(|ctx| ctx.canvas.size_y));
    vm.fire_event(hud, "PostRender", vec![c]);
    natives::with_context(|ctx| {
        let mut items = std::mem::take(&mut ctx.canvas.items);
        let hud_items = items.len();
        items.extend(before);
        ctx.canvas.items = items;
        HUD_ITEMS.with(|h| h.set(hud_items));
    });
}

thread_local! {
    /// Canvas items the HUD drew last frame (smoke test check).
    static HUD_ITEMS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Canvas items `myHUD.PostRender` produced in the last frame.
pub(crate) fn last_hud_items() -> usize {
    HUD_ITEMS.with(|h| h.get())
}
