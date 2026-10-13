//! p2menu: boots the POSTAL 2 menu system from the original compiled scripts and renders it
//! through the Canvas natives.
//!
//! A minimal standalone host: a uscript VM over the install's System directory with the shipped
//! inis as config, a world with the ini `DefaultGame` GameInfo, a PlayerController, and the engine
//! interaction graph (Console + `BaseMenu` = Shell.ShellRootWindow) built as the engine does. The
//! menu's `ShowMenu` is called as at startup; every frame the interactions get `Process_Tick` and
//! `Process_PreRender`/`Process_PostRender` with a script Canvas, and the Canvas natives draw into
//! the p2canvas renderer. Keyboard/mouse go through p2input to `Process_KeyEvent`.
//!
//! ```text
//! p2menu [--game <dir>] [--screenshot out.png] [--size 1024x768] [--frames N] [-v]
//! ```

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use p2canvas::natives::{self, CanvasContext, MenuClasses, MenuHost};
use p2canvas::shell::ShellNatives;
use p2canvas::{CanvasPlugin, CanvasStats, HudCanvas};
use p2input::{Action, Bindings, Input, Key};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use upkg::library::Library;
use uscript::config::ConfigSource;
use uscript::host::{Host, NativeCall};
use uscript::vm::{MissingNativePolicy, ObjId, Value, Vm};

const DEFAULT_GAME: &str = r"G:\SteamLibrary\steamapps\common\POSTAL2Complete";

#[derive(Resource, Clone)]
struct Args {
    game: PathBuf,
    screenshot: Option<PathBuf>,
    size: (u32, u32),
    verbose: bool,
    warmup: u32,
    script: Vec<(u32, Step)>,
}

/// One scripted input step (`--script "40:move 512,307;41:click;80:shot out/a.png"`).
#[derive(Clone, Debug)]
enum Step {
    Move(f32, f32),
    Press(Key),
    Release(Key),
    /// Press then release on the next frame.
    Tap(Key),
    Type(String),
    Shot(PathBuf),
    Exit,
}

fn parse_script(s: &str) -> Vec<(u32, Step)> {
    let mut out = Vec::new();
    for item in s.split(';').map(str::trim).filter(|x| !x.is_empty()) {
        let Some((f, cmd)) = item.split_once(':') else { continue };
        let Ok(f) = f.trim().parse::<u32>() else { continue };
        let cmd = cmd.trim();
        let (verb, rest) = cmd.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((cmd, ""));
        let key = || Key::from_name(rest);
        let step = match verb.to_ascii_lowercase().as_str() {
            "move" => rest.split_once(',').and_then(|(x, y)| Some(Step::Move(x.trim().parse().ok()?, y.trim().parse().ok()?))),
            "click" => Some(Step::Tap(Key::LEFT_MOUSE)),
            "press" => key().map(Step::Press),
            "release" => key().map(Step::Release),
            "key" | "tap" => key().map(Step::Tap),
            "type" => Some(Step::Type(rest.to_string())),
            "shot" => Some(Step::Shot(PathBuf::from(rest))),
            "exit" => Some(Step::Exit),
            _ => None,
        };
        match step {
            Some(s) => out.push((f, s)),
            None => eprintln!("bad script step: {item}"),
        }
    }
    out
}

fn parse_args() -> Args {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    Args {
        game: get("--game").map(PathBuf::from).or_else(|| std::env::var_os("POSTAL2_DIR").map(PathBuf::from)).unwrap_or_else(|| DEFAULT_GAME.into()),
        screenshot: get("--screenshot").map(PathBuf::from),
        size: get("--size").and_then(|s| s.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))).unwrap_or((1024, 768)),
        verbose: a.iter().any(|x| x == "-v"),
        warmup: get("--frames").and_then(|s| s.parse().ok()).unwrap_or(30),
        script: get("--script").map(|s| parse_script(&s)).unwrap_or_default(),
    }
}

// ------------------------------------------------------------------ config

/// The shipped inis as the VM's base config layer.
struct IniSource {
    /// file (lower) -> section (lower) -> [(key, value)] in file order.
    files: HashMap<String, HashMap<String, Vec<(String, String)>>>,
}

impl IniSource {
    fn load(system: &Path) -> Self {
        let mut files = HashMap::new();
        for name in ["Postal2.ini", "User.ini"] {
            let Ok(bytes) = std::fs::read(system.join(name)) else { continue };
            let text = p2input::bindings::decode_ini(&bytes);
            let mut secs: HashMap<String, Vec<(String, String)>> = HashMap::new();
            let mut cur = String::new();
            for l in text.lines() {
                let t = l.trim();
                if t.starts_with('[') && t.ends_with(']') {
                    cur = t[1..t.len() - 1].trim().to_ascii_lowercase();
                    continue;
                }
                if t.is_empty() || t.starts_with(';') {
                    continue;
                }
                if let Some((k, v)) = t.split_once('=') {
                    secs.entry(cur.clone()).or_default().push((k.trim().to_string(), v.to_string()));
                }
            }
            files.insert(name.to_ascii_lowercase(), secs);
        }
        Self { files }
    }

    fn get(&self, file: &str, section: &str, key: &str) -> Option<String> {
        self.values(file, section, key).into_iter().next().map(|v| v.1)
    }
}

impl ConfigSource for IniSource {
    fn values(&self, file: &str, section: &str, key: &str) -> Vec<(usize, String)> {
        let Some(secs) = self.files.get(&file.to_ascii_lowercase()) else { return vec![] };
        let Some(lines) = secs.get(&section.to_ascii_lowercase()) else { return vec![] };
        let mut out: Vec<(usize, String)> = Vec::new();
        let mut next = 0;
        for (k, v) in lines {
            let (name, idx) = match (k.find('['), k.ends_with(']')) {
                (Some(o), true) => (&k[..o], k[o + 1..k.len() - 1].trim().parse::<usize>().ok()),
                _ => (k.as_str(), None),
            };
            if !name.trim().eq_ignore_ascii_case(key) {
                continue;
            }
            let i = idx.unwrap_or(next);
            next = i + 1;
            out.retain(|(j, _)| *j != i);
            out.push((i, v.clone()));
        }
        out
    }
}

// ------------------------------------------------------------------ host

/// World services for the menu: the startup map name, console commands, resource properties.
struct MenuWorldHost {
    db: Arc<p2canvas::eval::MaterialDb>,
    input: Rc<RefCell<Input>>,
    size: (u32, u32),
    map: String,
    shell: Rc<ShellNatives>,
}

impl Host for MenuWorldHost {
    fn map_name(&self, _vm: &mut Vm) -> String {
        self.map.clone()
    }

    fn native(&self, vm: &mut Vm, call: &NativeCall, args: &mut Vec<Value>) -> Option<uscript::Result<Value>> {
        self.shell.native(vm, call, args)
    }

    fn console_command(&self, _vm: &mut Vm, _from: Option<ObjId>, command: &str) -> String {
        if let Some(r) = self.input.borrow_mut().console_query(command) {
            return r;
        }
        let up = command.trim().to_ascii_uppercase();
        let verb = up.split_whitespace().next().unwrap_or("");
        match verb {
            "GETFULLSCREEN" => "0".into(),
            "GETCURRENTRES" => format!("{}x{}", self.size.0, self.size.1),
            "GETCURRENTCOLORDEPTH" => "32".into(),
            "GETCURRENTTICKRATE" => "60".into(),
            // Level change requests (the game's ConsoleCommand("OPEN url") / "START url").
            "OPEN" | "START" | "SERVERTRAVEL" => {
                let url = command.trim()[verb.len()..].trim().to_string();
                self.shell.travel.borrow_mut().push(url);
                String::new()
            }
            _ => String::new(),
        }
    }

    fn resource_prop(&self, _vm: &mut Vm, path: &str, prop: &str) -> Option<Value> {
        natives::resource_prop(&self.db, path, prop)
    }
}

// ------------------------------------------------------------------ menu runtime

struct Menu {
    vm: Vm,
    host: MenuHost,
    input: Rc<RefCell<Input>>,
    frames: u32,
    verbose: bool,
    shell: Rc<ShellNatives>,
    /// Last travel request (this standalone host has no level loader; it reports and keeps going).
    travel: Option<String>,
}

fn boot(args: &Args, lib: Arc<Library>) -> Menu {
    let system = args.game.join("System");
    let ini = IniSource::load(&system);
    let mut vm = Vm::new(&system).expect("VM init (Core.u)");
    vm.missing_native_policy = MissingNativePolicy::Lenient;
    let engine = |k: &str| ini.get("Postal2.ini", "Engine.Engine", k);
    let classes = MenuClasses { console: engine("Console"), base_menu: engine("BaseMenu"), default_player_menu: None };
    let game_class = engine("DefaultGame").unwrap_or_else(|| "Engine.GameInfo".into());
    let startup = ini.get("Postal2.ini", "URL", "LocalMap").unwrap_or_else(|| "Startup.fuk".into());
    let startup = startup.trim_end_matches(".fuk").to_string();
    let user_bytes = std::fs::read(system.join("User.ini")).unwrap_or_default();
    let input = Rc::new(RefCell::new(Input::new(Bindings::from_ini(&p2input::bindings::decode_ini(&user_bytes)))));
    vm.set_config_source(Box::new(ini));
    // Saved config goes to the user dir (never into the install); keep it in memory for the demo.
    vm.config.user_dir = None;

    let mut ctx = CanvasContext::new(lib.clone(), args.size.0 as i32, args.size.1 as i32);
    let qinput = input.clone();
    ctx.console = Some(Box::new(move |c: &str| qinput.borrow_mut().console_query(c)));
    let db = ctx.db.clone();
    natives::install(ctx);
    natives::register(&mut vm);
    let shell = Rc::new(ShellNatives::new(&args.game));
    vm.set_host(Rc::new(MenuWorldHost { db, input: input.clone(), size: args.size, map: startup.clone(), shell: shell.clone() }));

    // World: LevelInfo + GameInfo (InitGame and begin-play events), then the local player.
    let game = vm.init_world(Some(&game_class)).map_err(|e| eprintln!("init_world({game_class}): {e}")).ok();
    if let Some(l) = vm.level {
        vm.set_prop(l, "TimeDilation", Value::Float(1.0));
    }
    // No map is loaded here; the startup map provides a PlayerStart, so place one for Login.
    if let Ok(ps) = vm.find_class("Engine", "PlayerStart") {
        vm.spawn_actor(ps, Default::default(), true);
    }
    // The viewport and its interactions exist before the player logs in (the engine's
    // SpawnPlayActor): Login spawns the controller, SetPlayer links it, then PostLogin possesses
    // the pawn (P2Player.Possess adds the load/map screens to the viewport).
    let mut host = MenuHost::new(&mut vm, &classes, None).expect("interaction master");
    let pc = game.and_then(|g| {
        let r = vm.fire_event(g, "Login", vec![Value::Str(String::new()), Value::Str("?Name=Dude".into()), Value::Str(String::new())]);
        r.and_then(|v| v.as_obj()).inspect(|&pc| {
            host.attach_player(&mut vm, pc);
            vm.fire_event(g, "PostLogin", vec![Value::Object(Some(pc))]);
            // The engine notifies the player once the level is entered (creates the GameState).
            vm.fire_event(pc, "TravelPostAccept", vec![]);
        })
    });
    if pc.is_none() {
        if let Some(pc) = vm.find_class("Engine", "PlayerController").ok().and_then(|c| vm.spawn_actor(c, Default::default(), true)) {
            host.attach_player(&mut vm, pc);
        }
    }
    let pc = host.player_controller;
    if std::env::var_os("P2MENU_PROBE").is_some() {
        if let Ok(c) = vm.find_class("GameTypes", "AWPGameInfo") {
            let d = vm.default_value(c, "Days").cloned();
            println!("PROBE AWPGameInfo.Days = {:?}", d.map(|v| format!("{v:?}").chars().take(400).collect::<String>()));
        }
        if let Some(g) = game {
            let d = vm.get_prop(g, "Days").cloned();
            println!("PROBE game.Days = {:?}", d.map(|v| format!("{v:?}").chars().take(400).collect::<String>()));
        }
    }
    println!("BOOT game={game_class} startup={startup} player_controller={:?} console={:?} base_menu={:?}", pc.map(|p| vm.object_name(p)), classes.console, classes.base_menu);
    println!("MENU console={:?} root={:?}", host.console.map(|o| vm.object_name(o)), host.base_menu.map(|o| vm.object_name(o)));
    Menu { vm, host, input, frames: 0, verbose: args.verbose, shell, travel: None }
}

impl Menu {
    /// The engine shows the main menu on the startup map once the viewport has rendered (the
    /// root window learns its size in its first PostRender; P2GameInfo calls MenuMode/ShowMenu).
    fn show_menu(&mut self) {
        if let Some(root) = self.host.base_menu {
            let c = self.vm.objects[root].class;
            if let Err(e) = self.vm.call(c, Some(root), "ShowMenu", vec![]) {
                eprintln!("ShowMenu: {e}");
            }
            println!("ROOT state={}", self.vm.state_name(root));
        }
    }

    fn frame(&mut self, dt: f32, w: i32, h: i32) {
        if self.frames == 1 {
            self.show_menu();
        }
        if let Some(l) = self.vm.level {
            let t = self.vm.get_prop(l, "TimeSeconds").map(|v| v.as_float()).unwrap_or(0.0) + dt;
            self.vm.set_prop(l, "TimeSeconds", Value::Float(t));
            self.vm.set_prop(l, "TimeSecondsAlways", Value::Float(t));
        }
        self.vm.tick_world(dt);
        self.host.frame(&mut self.vm, dt, w, h);
        while let Some(url) = self.shell.take_travel() {
            println!("TRAVEL {url}");
            self.travel = Some(url);
        }
        self.frames += 1;
        if self.frames == 3 || (self.verbose && self.frames % 120 == 0) {
            self.report();
        }
    }

    fn key(&mut self, key: Key, action: Action, delta: f32) {
        if !self.input.borrow_mut().pre_process(key, action) {
            return;
        }
        if !self.host.key_event(&mut self.vm, key.0, action as u8, delta) {
            let mut t = PcTarget { vm: &mut self.vm, pc: self.host.player_controller };
            self.input.borrow_mut().process(&mut t, key, action, delta);
        }
    }

    fn report(&mut self) {
        if let Some(root) = self.host.base_menu {
            let vm = &self.vm;
            let show = |o: ObjId| {
                let g = |n: &str| vm.get_prop(o, n).map(|v| vm.value_string(v)).unwrap_or_default();
                format!(
                    "{} state={} vis={} active={} L={} T={} W={} H={}",
                    vm.object_name(o),
                    vm.state_name(o),
                    g("bWindowVisible"),
                    g("bUWindowActive"),
                    g("WinLeft"),
                    g("WinTop"),
                    g("WinWidth"),
                    g("WinHeight")
                )
            };
            println!("  root: {}", show(root));
            let vo = vm.get_prop(root, "ViewportOwner").and_then(|v| v.as_obj());
            let pc = vo.and_then(|v| vm.get_prop(v, "Actor")).and_then(|v| v.as_obj());
            let hud = pc.and_then(|p| vm.get_prop(p, "myHUD")).and_then(|v| v.as_obj());
            println!(
                "  viewport={:?} pc={:?} hud={:?}",
                vo.map(|o| vm.object_name(o)),
                pc.map(|o| vm.object_name(o)),
                hud.map(|o| vm.object_name(o))
            );
            let mut stack = vec![(root, 1)];
            let mut n = 0;
            while let Some((w, d)) = stack.pop() {
                let mut c = vm.get_prop(w, "FirstChildWindow").and_then(|v| v.as_obj());
                while let Some(ch) = c {
                    n += 1;
                    if n > 60 {
                        break;
                    }
                    println!("  {}{}", "  ".repeat(d), show(ch));
                    stack.push((ch, d + 1));
                    c = vm.get_prop(ch, "NextSiblingWindow").and_then(|v| v.as_obj());
                }
            }
        }
        let mut miss: Vec<_> = self.vm.missing_natives.iter().collect();
        miss.sort_by(|a, b| b.1.cmp(a.1));
        let mut errs: Vec<_> = self.vm.script_errors.iter().collect();
        errs.sort_by(|a, b| b.1.cmp(a.1));
        println!("FRAME {}: missing natives {} distinct, script errors {} distinct", self.frames, miss.len(), errs.len());
        for (k, n) in miss.iter().take(if self.verbose { 40 } else { 12 }) {
            println!("  missing {n:6} {k}");
        }
        for (k, n) in errs.iter().take(if self.verbose { 20 } else { 6 }) {
            println!("  error   {n:6} {k}");
        }
        let none = self.vm.log.iter().filter(|l| l.contains("Accessed None")).count();
        if std::env::var_os("P2MENU_NONE_CTX").is_some() {
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for l in &self.vm.log {
                *counts.entry(l.as_str()).or_default() += 1;
            }
            let mut v: Vec<_> = counts.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            for (l, n) in v.into_iter().take(12) {
                println!("  logcount {n:6} {l}");
            }
        }
        println!("  log lines {} (Accessed None {none})", self.vm.log.len());
        if self.verbose {
            let mut seen = std::collections::HashSet::new();
            for l in self.vm.log.iter().filter(|l| seen.insert(l.as_str())).take(40) {
                println!("  log: {l}");
            }
        }
        self.vm.log.clear();
    }
}

/// The PlayerController as an input target (input byte/float vars, exec functions).
struct PcTarget<'a> {
    vm: &'a mut Vm,
    pc: Option<ObjId>,
}

impl p2input::InputTarget for PcTarget<'_> {
    fn input_byte(&mut self, name: &str) -> Option<u8> {
        self.pc.and_then(|p| self.vm.get_prop(p, name)).map(|v| v.as_int() as u8)
    }
    fn set_input_byte(&mut self, name: &str, v: u8) {
        if let Some(p) = self.pc {
            self.vm.set_prop(p, name, Value::Byte(v));
        }
    }
    fn input_float(&mut self, name: &str) -> Option<f32> {
        self.pc.and_then(|p| self.vm.get_prop(p, name)).map(|v| v.as_float())
    }
    fn set_input_float(&mut self, name: &str, v: f32) {
        if let Some(p) = self.pc {
            self.vm.set_prop(p, name, Value::Float(v));
        }
    }
    fn exec(&mut self, command: &str) -> bool {
        let Some(p) = self.pc else { return false };
        let mut it = command.split_whitespace();
        let Some(f) = it.next() else { return false };
        let c = self.vm.objects[p].class;
        let args: Vec<Value> = it.map(|s| Value::Str(s.into())).collect();
        self.vm.call(c, Some(p), f, args).is_ok()
    }
}

// ------------------------------------------------------------------ bevy

fn main() {
    let args = parse_args();
    let lib = Arc::new(Library::new(&args.game).unwrap_or_else(|e| panic!("cannot index game install at {}: {e}", args.game.display())));
    let menu = boot(&args, lib.clone());
    let capture = args.screenshot.is_some();
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "p2menu - POSTAL 2 menus from script".into(),
                resolution: bevy::window::WindowResolution::new(args.size.0, args.size.1).with_scale_factor_override(1.0),
                present_mode: if capture { bevy::window::PresentMode::AutoNoVsync } else { bevy::window::PresentMode::AutoVsync },
                ..default()
            }),
            ..default()
        }))
        .add_plugins(CanvasPlugin { lib, clear: Some(Color::BLACK) })
        .insert_non_send(menu)
        .insert_resource(args)
        .init_resource::<Frames>()
        .init_resource::<Driver>()
        .add_systems(Update, (run_menu, capture_system).chain())
        .run();
}

#[derive(Resource, Default)]
struct Frames(u32);

/// Scripted-input state: the virtual cursor (overrides the OS cursor once a `move` ran) and keys
/// to release next frame.
#[derive(Resource, Default)]
struct Driver {
    mouse: Option<(f32, f32)>,
    release: Vec<Key>,
    shots: Vec<PathBuf>,
    exit: bool,
    exit_at: Option<u32>,
    last_os: Option<(f32, f32)>,
}

#[allow(clippy::too_many_arguments)]
fn run_menu(
    mut menu: NonSendMut<Menu>,
    mut hud: ResMut<HudCanvas>,
    mut drv: ResMut<Driver>,
    args: Res<Args>,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
) {
    let Ok(win) = windows.single() else { return };
    let (w, h) = (win.physical_width() as i32, win.physical_height() as i32);
    let frame = menu.frames;
    // Scripted steps for this frame.
    let mut taps = Vec::new();
    for k in std::mem::take(&mut drv.release) {
        menu.key(k, Action::Release, 0.0);
    }
    for (_, step) in args.script.iter().filter(|(f, _)| *f == frame) {
        match step {
            Step::Move(x, y) => drv.mouse = Some((*x, *y)),
            Step::Press(k) => menu.key(*k, Action::Press, 0.0),
            Step::Release(k) => menu.key(*k, Action::Release, 0.0),
            Step::Tap(k) => taps.push(*k),
            Step::Type(s) => {
                for ch in s.chars() {
                    let m = &mut *menu;
                    m.host.key_type(&mut m.vm, ch as u32 as u8, &ch.to_string());
                }
            }
            Step::Shot(p) => drv.shots.push(p.clone()),
            Step::Exit => drv.exit = true,
        }
    }
    let os_cursor = win.cursor_position().map(|p| (p.x, p.y));
    if os_cursor != drv.last_os {
        // A real mouse move takes over from scripted/warped positions.
        if drv.last_os.is_some() && os_cursor.is_some() {
            drv.mouse = None;
        }
        drv.last_os = os_cursor;
    }
    let cursor = drv.mouse.or(os_cursor);
    if let Some((x, y)) = cursor {
        let m = &mut *menu;
        m.host.set_mouse(&mut m.vm, x, y);
    }
    for k in taps {
        menu.key(k, Action::Press, 0.0);
        drv.release.push(k);
    }
    for e in p2input::bevy_map::collect_events(&keys, &mouse, motion.delta, 0.0, &[]) {
        if e.key.is_axis() {
            continue; // the UWindow cursor follows the OS cursor (WindowsMouseX/Y)
        }
        menu.key(e.key, e.action, e.delta);
    }
    let dt = time.delta_secs().clamp(0.0, 0.1);
    menu.frame(dt, w, h);
    {
        let m = &mut *menu;
        if let Some(p) = m.host.take_mouse_warp(&mut m.vm) {
            drv.mouse = Some(p);
        }
        m.host.draw_cursor(&m.vm);
    }
    // Move this frame's draws from the natives' canvas into the renderer.
    let items = natives::with_context(|ctx| std::mem::take(&mut ctx.canvas.items));
    if std::env::var_os("P2CANVAS_TRACE").is_some() && menu.frames == 3 {
        for it in &items {
            println!("  draw {}.{} at ({:.0},{:.0}) {}x{} uv ({:.2},{:.2})-({:.2},{:.2}) style {} color {:?}", it.material.pkg.name, it.material.name(), it.x, it.y, it.w, it.h, it.u0, it.v0, it.u1, it.v1, it.style, it.color.0);
        }
    }
    hud.0.items = items;
}

fn capture_system(mut commands: Commands, args: Res<Args>, mut frames: ResMut<Frames>, mut drv: ResMut<Driver>, stats: Res<CanvasStats>, mut exit: MessageWriter<AppExit>) {
    frames.0 += 1;
    if frames.0 == 5 {
        println!("CANVAS batches={} quads={} materials={} images={}", stats.batches, stats.quads, stats.materials, stats.images);
    }
    for p in std::mem::take(&mut drv.shots) {
        println!("SHOT {} at frame {}", p.display(), frames.0);
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(p));
    }
    if drv.exit {
        drv.exit = false;
        // Give pending screenshots a few frames to land.
        drv.exit_at = Some(frames.0 + 20);
    }
    if drv.exit_at.is_some_and(|f| frames.0 >= f) {
        exit.write(AppExit::Success);
    }
    let Some(path) = &args.screenshot else { return };
    if frames.0 == args.warmup {
        println!("CANVAS batches={} quads={} materials={} images={}", stats.batches, stats.quads, stats.materials, stats.images);
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path.clone()));
    }
    if frames.0 > args.warmup + 10 && (path.exists() || frames.0 > args.warmup + 600) {
        exit.write(AppExit::Success);
    }
}
