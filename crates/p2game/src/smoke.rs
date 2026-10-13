//! `p2game --smoke`: the end-to-end flow, driven headlessly with scripted input.
//!
//! Stages (each records frames, script errors and missing natives that appeared during it):
//! 1. `boot`: the startup map boots and the shell's main menu comes up.
//! 2. `menu`: New Game is clicked through the shell menus (main menu `NewChoice`, game mode
//!    `P2Choice`, start menu `StartMF` / `StartChoice`, controls screen `StartChoice`) with real
//!    mouse events at each choice's on-screen centre, until script requests a travel.
//! 3. `intro`: the travel lands on the intro map (`?Game=GameSinglePlayer`). Its Matinee chain
//!    (IntroScene, IntroScene2, TrailerScene, TrailerScene2, MapScene, about 234 s) ends with the
//!    `DudeStartsGame` ScriptedTrigger sending the player to the first day. The driver skips the
//!    skippable scenes as a player would (`PlayerController.bWantsToSkip`). After `--intro-secs`
//!    (default 120) it calls `SendPlayerToFirstDay` itself (noted as forced; a failure with
//!    `P2GAME_SMOKE_STRICT=1`).
//! 4. `day`: the first day map is loaded, the player has a pawn and the HUD draws.
//! 5. `walk`: W is held for 5 s, and the pawn must move.
//! 6. `fire`: the fire button is pressed once.
//! 7. `portal`: the player travels through a level portal (`open <next map>#<portal>`, what a
//!    portal Teleporter's Touch does through `ClientTravel`) and must arrive with a live pawn and
//!    no host state still pointing at the previous level's objects (the object table is truncated
//!    and refilled on travel). Default `Suburbs-1#pod1a` from suburbs-3; override with
//!    `P2GAME_SMOKE_PORTAL`.
//!
//! The same [`SmokeDriver`] can feed the windowed mode (stage screenshots).

use crate::frontend::{FrameInput, Frontend};
use p2game::boot::Game;
use p2input::bevy_map::InputEvent;
use p2input::{Action, Key};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use uscript::vm::{ObjId, Value, Vm};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Stage {
    Boot,
    Menu,
    Intro,
    Day,
    Walk,
    Fire,
    Portal,
    Done,
}

impl Stage {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Stage::Boot => "boot",
            Stage::Menu => "menu",
            Stage::Intro => "intro",
            Stage::Day => "day",
            Stage::Walk => "walk",
            Stage::Fire => "fire",
            Stage::Portal => "portal",
            Stage::Done => "done",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct StageResult {
    pub stage: Stage,
    pub ok: bool,
    pub frames: u32,
    pub map: String,
    pub notes: Vec<String>,
    pub new_errors: BTreeMap<String, u64>,
    pub new_natives: BTreeMap<String, u64>,
}

/// Scripted input and checks for the smoke flow; call [`SmokeDriver::input`] before each front
/// end frame and [`SmokeDriver::after`] after it.
pub(crate) struct SmokeDriver {
    pub stage: Stage,
    frames_in_stage: u32,
    pub results: Vec<StageResult>,
    notes: Vec<String>,
    errors_at_start: BTreeMap<String, u64>,
    natives_at_start: BTreeMap<String, u64>,
    /// Pending scripted events (frame offset from now, event).
    queue: Vec<(u32, InputEvent)>,
    mouse: (f32, f32),
    clicks: u32,
    last_menu: Option<ObjId>,
    walk_start: Option<[f32; 3]>,
    intro_secs: f32,
    forced_day: bool,
    pub dt: f32,
    /// Stage that just finished this frame (for screenshots).
    pub finished: Option<Stage>,
    /// Map the portal stage left from.
    portal_from: Option<String>,
}

const MENU_CHOICES: &[&str] = &["NewChoice", "P2Choice", "StartMF", "StartChoice"];

fn ev(key: Key, action: Action) -> InputEvent {
    InputEvent { key, action, delta: 0.0 }
}

impl SmokeDriver {
    pub(crate) fn new(intro_secs: f32) -> Self {
        SmokeDriver {
            stage: Stage::Boot,
            frames_in_stage: 0,
            results: Vec::new(),
            notes: Vec::new(),
            errors_at_start: BTreeMap::new(),
            natives_at_start: BTreeMap::new(),
            queue: Vec::new(),
            mouse: (0.0, 0.0),
            clicks: 0,
            last_menu: None,
            walk_start: None,
            intro_secs,
            forced_day: false,
            dt: 1.0 / 30.0,
            finished: None,
            portal_from: None,
        }
    }

    fn note(&mut self, s: impl Into<String>) {
        let s = s.into();
        println!("smoke [{}] {s}", self.stage.name());
        self.notes.push(s);
    }

    fn finish(&mut self, game: &Game, ok: bool) {
        let vm = &game.world.vm;
        let diff = |now: &std::collections::HashMap<String, u64>, before: &BTreeMap<String, u64>| -> BTreeMap<String, u64> {
            now.iter().filter_map(|(k, v)| {
                let d = v.saturating_sub(*before.get(k).unwrap_or(&0));
                (d > 0).then(|| (k.clone(), d))
            }).collect()
        };
        let r = StageResult {
            stage: self.stage,
            ok,
            frames: self.frames_in_stage,
            map: game.level.map.name.clone(),
            notes: std::mem::take(&mut self.notes),
            new_errors: diff(&vm.script_errors.iter().map(|(k, v)| (k.clone(), *v)).collect(), &self.errors_at_start),
            new_natives: diff(&vm.missing_natives.iter().map(|(k, v)| (k.clone(), *v)).collect(), &self.natives_at_start),
        };
        println!(
            "smoke: stage {:<6} {} in {} frames on {} ({} new script errors, {} new missing natives)",
            r.stage.name(),
            if ok { "PASS" } else { "FAIL" },
            r.frames,
            r.map,
            r.new_errors.values().sum::<u64>(),
            r.new_natives.values().sum::<u64>()
        );
        self.results.push(r);
        self.finished = Some(self.stage);
        self.stage = if ok {
            match self.stage {
                Stage::Boot => Stage::Menu,
                Stage::Menu => Stage::Intro,
                Stage::Intro => Stage::Day,
                Stage::Day => Stage::Walk,
                Stage::Walk => Stage::Fire,
                Stage::Fire => Stage::Portal,
                Stage::Portal | Stage::Done => Stage::Done,
            }
        } else {
            Stage::Done
        };
        self.frames_in_stage = 0;
        self.snapshot(game);
    }

    fn snapshot(&mut self, game: &Game) {
        let vm = &game.world.vm;
        self.errors_at_start = vm.script_errors.iter().map(|(k, v)| (k.clone(), *v)).collect();
        self.natives_at_start = vm.missing_natives.iter().map(|(k, v)| (k.clone(), *v)).collect();
    }

    /// Mouse position and events for this frame.
    pub(crate) fn input(&mut self, game: &mut Game, front: &Frontend) -> (Vec<InputEvent>, (f32, f32)) {
        self.finished = None;
        if self.stage == Stage::Boot && self.frames_in_stage == 0 && self.results.is_empty() {
            self.snapshot(game);
        }
        let mut out = Vec::new();
        self.queue.retain_mut(|(n, e)| {
            if *n == 0 {
                out.push(*e);
                false
            } else {
                *n -= 1;
                true
            }
        });
        match self.stage {
            // Start clicking once the main menu has been on screen a moment (stage screenshot).
            Stage::Menu if self.queue.is_empty() && self.frames_in_stage >= 100 && self.frames_in_stage % 20 == 10 => self.click_next_choice(game, front),
            Stage::Walk if self.frames_in_stage == 1 => out.push(ev(Key::letter('W').unwrap(), Action::Press)),
            Stage::Fire if self.frames_in_stage == 1 => out.push(ev(Key::LEFT_MOUSE, Action::Press)),
            Stage::Fire if self.frames_in_stage == 6 => out.push(ev(Key::LEFT_MOUSE, Action::Release)),
            // Skip the intro cutscenes (the player pressing the skip key).
            Stage::Intro => {
                if let Some(pc) = game.player_controller {
                    game.world.vm.set_prop(pc, "bWantsToSkip", Value::Byte(1));
                }
            }
            _ => {}
        }
        (out, self.mouse)
    }

    /// Clicks the next New Game choice of the current shell menu (`ShellRootWindow.MyMenu`).
    fn click_next_choice(&mut self, game: &mut Game, front: &Frontend) {
        let vm = &mut game.world.vm;
        let Some(root) = front.root() else { return };
        let Some(menu) = vm.get_prop(root, "MyMenu").and_then(|v| v.as_obj()) else {
            self.note("no ShellRootWindow.MyMenu");
            return;
        };
        let menu_class = vm.classes[vm.objects[menu].class].name.clone();
        let Some((prop, choice)) = MENU_CHOICES.iter().find_map(|p| vm.get_prop(menu, p).and_then(|v| v.as_obj()).filter(|c| !vm.objects[*c].deleted).map(|c| (*p, c))) else {
            if self.last_menu != Some(menu) {
                self.note(format!("menu {menu_class} has none of {MENU_CHOICES:?}"));
            }
            self.last_menu = Some(menu);
            return;
        };
        if self.last_menu == Some(menu) && self.clicks > 0 && self.frames_in_stage % 120 == 10 {
            // Still the same menu after several clicks: the click did not land; drive the
            // control's Click directly (the same handler the mouse-up calls).
            self.note(format!("{menu_class}.{prop}: mouse click had no effect, calling Click directly"));
            let c = vm.objects[choice].class;
            if let Err(e) = vm.call(c, Some(choice), "Click", vec![Value::Float(1.0), Value::Float(1.0)]) {
                vm.record_error("smoke Click", &e);
            }
            return;
        }
        let (x, y) = window_center(vm, choice, root);
        if self.last_menu != Some(menu) {
            self.note(format!("{menu_class}: clicking {prop} at ({x:.0},{y:.0})"));
        }
        self.last_menu = Some(menu);
        self.mouse = (x, y);
        self.clicks += 1;
        // Move first (MouseEnter / MouseWindow), then press and release a few frames later.
        self.queue.push((2, ev(Key::LEFT_MOUSE, Action::Press)));
        self.queue.push((4, ev(Key::LEFT_MOUSE, Action::Release)));
    }

    /// Checks after a frame: stage completion and timeouts. `traveled` is true if the game changed
    /// level this frame.
    pub(crate) fn after(&mut self, game: &mut Game, front: &Frontend, traveled: bool, menu_up: bool) {
        self.frames_in_stage += 1;
        let secs = self.frames_in_stage as f32 * self.dt;
        let map = game.level.map.name.to_ascii_lowercase();
        let pawn = game.player_controller.and_then(|pc| game.world.vm.get_prop(pc, "Pawn")).and_then(|v| v.as_obj()).filter(|p| !game.world.vm.objects[*p].deleted);
        match self.stage {
            Stage::Boot => {
                if menu_up {
                    self.note(format!("main menu up ({})", front.root().map(|r| game.world.vm.object_name(r)).unwrap_or_default()));
                    self.finish(game, true);
                } else if secs > 10.0 {
                    self.note("main menu never came up");
                    self.finish(game, false);
                }
            }
            Stage::Menu => {
                if traveled {
                    self.note(format!("traveled to {}", game.level.map.name));
                    self.finish(game, true);
                } else if secs > 60.0 {
                    self.note("no travel after clicking through the menus");
                    self.finish(game, false);
                }
            }
            Stage::Intro => {
                if traveled && !map.contains("intro") {
                    self.note(format!("intro ended: now on {}{}", game.level.map.name, if self.forced_day { " (forced SendPlayerToFirstDay)" } else { "" }));
                    self.finish(game, !self.forced_day || std::env::var_os("P2GAME_SMOKE_STRICT").is_none());
                } else if traveled {
                    self.note(format!("traveled to {}", game.level.map.name));
                } else if secs > self.intro_secs && !self.forced_day {
                    self.forced_day = true;
                    self.note(format!("intro still running after {:.0}s: SendPlayerToFirstDay", secs));
                    if let (Some(g), Some(pc)) = (game.game_info, game.player_controller) {
                        let vm = &mut game.world.vm;
                        if let Err(e) = vm.call(vm.objects[g].class, Some(g), "SendPlayerToFirstDay", vec![Value::Object(Some(pc)), Value::Bool(true)]) {
                            vm.record_error("smoke SendPlayerToFirstDay", &e);
                        }
                    }
                } else if secs > self.intro_secs + 20.0 {
                    self.note("no travel to the first day");
                    self.finish(game, false);
                }
            }
            Stage::Day => {
                let hud = crate::frontend::last_hud_items();
                if pawn.is_some() && hud > 0 && self.frames_in_stage >= 30 {
                    let vm = &game.world.vm;
                    let p = pawn.unwrap();
                    self.note(format!("player pawn {} at {:?}, HUD {} items", vm.object_name(p), vm.actor_loc(p), hud));
                    self.finish(game, true);
                } else if secs > 15.0 {
                    self.note(format!("pawn {:?}, HUD items {hud}", pawn.map(|p| game.world.vm.object_name(p))));
                    self.finish(game, false);
                }
            }
            Stage::Walk => {
                let Some(p) = pawn else {
                    self.note("pawn gone");
                    return self.finish(game, false);
                };
                let loc = game.world.vm.actor_loc(p);
                let start = *self.walk_start.get_or_insert(loc);
                if secs >= 5.0 {
                    self.queue.push((0, ev(Key::letter('W').unwrap(), Action::Release)));
                    let d = ((loc[0] - start[0]).powi(2) + (loc[1] - start[1]).powi(2)).sqrt();
                    self.note(format!("walked {d:.0} units in 5 s ({:?} -> {:?}, physics {:?})", start, loc, game.world.vm.get_prop(p, "Physics")));
                    self.finish(game, d > 50.0);
                }
            }
            Stage::Fire => {
                if self.frames_in_stage == 30 {
                    let vm = &game.world.vm;
                    let w = pawn.and_then(|p| vm.get_prop(p, "Weapon")).and_then(|v| v.as_obj());
                    let state = w.map(|w| vm.state_name(w));
                    let ammo = w.and_then(|w| vm.get_prop(w, "AmmoType")).and_then(|v| v.as_obj()).and_then(|a| vm.get_prop(a, "AmmoAmount")).cloned();
                    self.note(format!("weapon {:?} state {:?} ammo {:?}", w.map(|w| vm.object_name(w)), state, ammo));
                    self.finish(game, w.is_some());
                }
            }
            Stage::Portal => {
                if self.frames_in_stage == 1 {
                    let target = std::env::var("P2GAME_SMOKE_PORTAL").unwrap_or_else(|_| "Suburbs-1#pod1a".into());
                    self.portal_from = Some(game.level.map.name.clone());
                    self.note(format!("open {target} from {}", game.level.map.name));
                    if let Some(pc) = game.player_controller {
                        crate::frontend::exec_command(&mut game.world.vm, pc, &format!("open {target}"));
                    }
                } else if traveled {
                    let vm = &game.world.vm;
                    let n = vm.objects.len();
                    // Host caches keyed by object id must not hold ids from before the travel. (Ids
                    // of actors destroyed this frame are pruned on the next animation tick.)
                    let stale_anims = game.host.anims.borrow().keys().filter(|&&a| a >= n).count();
                    for &a in game.host.anims.borrow().keys().filter(|&&a| a >= n) {
                        self.notes.push(format!("stale anim id {a} (object table len {n})"));
                    }
                    let pc_ok = game.player_controller.is_some_and(|pc| pc < n && !vm.objects[pc].deleted);
                    self.note(format!("arrived on {} (pawn {:?}, {} anim instances, {stale_anims} stale)", game.level.map.name, pawn.map(|p| vm.object_name(p)), game.host.anims.borrow().len()));
                    let moved = self.portal_from.as_deref().is_some_and(|f| !f.eq_ignore_ascii_case(&game.level.map.name));
                    self.finish(game, moved && pc_ok && stale_anims == 0);
                } else if secs > 20.0 {
                    self.note("no travel through the portal");
                    self.finish(game, false);
                }
            }
            Stage::Done => {}
        }
    }

    pub(crate) fn passed(&self) -> bool {
        self.results.len() == 7 && self.results.iter().all(|r| r.ok)
    }

    pub(crate) fn report(&self) -> String {
        let mut s = String::from("stage\tresult\tframes\tmap\tnew_script_errors\tnew_missing_natives\tnotes\n");
        for r in &self.results {
            let top = |m: &BTreeMap<String, u64>| {
                let mut v: Vec<_> = m.iter().collect();
                v.sort_by(|a, b| b.1.cmp(a.1));
                v.iter().take(8).map(|(k, n)| format!("{k} x{n}")).collect::<Vec<_>>().join("; ")
            };
            let _ = writeln!(
                s,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                r.stage.name(),
                if r.ok { "PASS" } else { "FAIL" },
                r.frames,
                r.map,
                top(&r.new_errors).replace(['\t', '\n'], " "),
                top(&r.new_natives),
                r.notes.join(" | ").replace(['\t', '\n'], " ")
            );
        }
        s
    }
}

/// Centre of a UWindow control in viewport pixels (`WindowToGlobal` times the root's GUIScale).
fn window_center(vm: &Vm, w: ObjId, root: ObjId) -> (f32, f32) {
    let f = |o: ObjId, n: &str| vm.get_prop(o, n).map(|v| v.as_float()).unwrap_or(0.0);
    let (mut x, mut y) = (f(w, "WinWidth") / 2.0, f(w, "WinHeight") / 2.0);
    let mut p = Some(w);
    for _ in 0..32 {
        let Some(c) = p else { break };
        if c == root {
            break;
        }
        x += f(c, "WinLeft");
        y += f(c, "WinTop");
        p = vm.get_prop(c, "ParentWindow").and_then(|v| v.as_obj());
    }
    let scale = Some(f(root, "GUIScale")).filter(|s| *s > 0.0).unwrap_or(1.0);
    (x * scale, y * scale)
}

/// Headless run of the whole flow. Returns the process exit code.
pub(crate) fn run(mut game: Game, game_root: &std::path::Path, out_dir: &std::path::Path, intro_secs: f32, max_frames: u32) -> i32 {
    let (w, h) = (1024, 768);
    let mut front = Frontend::install(&mut game, game_root, w, h, true);
    let mut drv = SmokeDriver::new(intro_secs);
    drv.snapshot(&game);
    let t0 = std::time::Instant::now();
    let dt = drv.dt;
    for _ in 0..max_frames {
        if drv.stage == Stage::Done {
            break;
        }
        game.tick(dt);
        game.drain_log(false);
        for l in p2game::save::process_requests(&mut game) {
            println!("smoke: {l}");
        }
        let (events, mouse) = drv.input(&mut game, &front);
        let out = front.frame(&mut game, FrameInput { events: &events, typed: &[], mouse: Some(mouse), width: w, height: h, dt });
        // Headless: drop this frame's draw list.
        p2canvas::natives::with_context(|ctx| ctx.canvas.items.clear());
        let mut traveled = false;
        if let Some((map, opts)) = game.take_travel() {
            println!("smoke: travel {map}{opts}");
            if game.travel(&map, &opts) {
                game.drain_log(false);
                traveled = true;
            } else {
                eprintln!("smoke: travel to {map} failed: map not found");
            }
        }
        drv.after(&mut game, &front, traveled, out.menu_up);
    }
    if drv.stage != Stage::Done {
        drv.note(format!("frame budget ({max_frames}) exhausted"));
        drv.finish(&game, false);
    }
    let report = drv.report();
    std::fs::create_dir_all(out_dir).ok();
    std::fs::write(out_dir.join("smoke.tsv"), &report).ok();
    println!("\n{report}");
    let ok = drv.passed();
    println!("smoke: {} in {:.1}s (report: {})", if ok { "PASSED" } else { "FAILED" }, t0.elapsed().as_secs_f32(), out_dir.join("smoke.tsv").display());
    if ok { 0 } else { 1 }
}
