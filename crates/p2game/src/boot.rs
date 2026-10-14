//! Boot sequence: GameInfo selection, level startup in UE2 order, and the per-frame tick loop.
//!
//! The level's actors are handed to the VM's world (`vm.actors`, `vm.level`) so that script
//! iterators, Spawn, Destroy and state code see the loaded map. Startup order for a freshly loaded
//! level (the order the engine uses, so script sees the same world state at each step):
//! 1. The GameInfo is spawned without events and stored in `LevelInfo.Game`.
//! 2. `LevelInfo.bBegunPlay = true`, `bStartup = true`, `TimeSeconds = 0`.
//! 3. `GameInfo.InitGame(Options, out Error)`.
//! 4. `PreBeginPlay` on every actor, then `BeginPlay` on every actor, then `PostBeginPlay`, then
//!    `PostNetBeginPlay`, then `SetInitialState`. Each pass covers the whole actor list before the
//!    next starts; actors destroyed along the way are skipped. Actors whose
//!    `bScriptInitialized` is already set are left alone.
//! 5. `bStartup = false`; the local player logs in (`Login`, then `PostLogin`).
//!
//! Each frame then runs `Vm::tick_world`: `TimeSeconds`, `Tick`, state code and latent actions,
//! `Timer`, `LifeSpan`, cleanup of destroyed actors.
//!
//! The VM runs with `MissingNativePolicy::Lenient`, so a missing native returns a zero value and
//! is counted in `vm.missing_natives` instead of aborting the event. Script errors are counted in
//! `vm.script_errors`. Both histograms are the priority list for native work.

use crate::level::{Level, World};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use uscript::host::SpawnParams;
use uscript::vm::{ClassId, MissingNativePolicy, ObjId, Value};

pub const BEGIN_PLAY_ORDER: [&str; 5] = ["PreBeginPlay", "BeginPlay", "PostBeginPlay", "PostNetBeginPlay", "SetInitialState"];

/// `P2GAME_BOOT_TIMING=1`: prints how long each level-startup stage takes (stderr).
fn boot_lap(t0: std::time::Instant, what: &str) {
    if std::env::var_os("P2GAME_BOOT_TIMING").is_some() {
        eprintln!("boot: {what} {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0);
    }
}

pub struct Game {
    pub world: World,
    pub level: Level,
    pub host: std::rc::Rc<crate::host::GameHost>,
    pub game_info: Option<ObjId>,
    pub game_class: String,
    pub player_controller: Option<ObjId>,
    pub accessed_none: u64,
    /// Accessed None warnings by message text (headless/sweep report).
    pub accessed_none_by: std::collections::BTreeMap<String, u64>,
    pub log_lines: u64,
    pub time: f32,
    /// Arrival portal of the current level (`Map#Portal`), handed to `GameInfo.Login`.
    pub portal: String,
    /// Increments every time a level is installed (boot, travel, save load). Host-side caches
    /// keyed by VM object ids must reset when it changes: the object table is truncated and
    /// refilled, so old ids are out of range or name different objects. (A map `Arc` address is
    /// not a safe key: the next map can be allocated at the freed address.)
    pub level_serial: u64,
}

impl Game {
    /// Picks the GameInfo class: explicit override, then the level's `DefaultGameType`, then the
    /// ini `DefaultGame`.
    fn game_class(world: &mut World, level: &Level, overridden: Option<&str>) -> Option<ClassId> {
        let mut candidates: Vec<String> = Vec::new();
        if let Some(o) = overridden {
            candidates.push(o.to_string());
        }
        if let Some(li) = level.level_info {
            if let Some(Value::Str(s)) = world.vm.get_prop(li, "DefaultGameType") {
                if !s.is_empty() {
                    candidates.push(s.clone());
                }
            }
        }
        if let Some(d) = &world.cfg.default_game {
            candidates.push(d.to_string());
        }
        candidates.iter().find_map(|c| crate::ini::ClassRef::parse(c).and_then(|r| world.class_ref(&r)))
    }

    /// Hands the level to the VM world and spawns the GameInfo (no events yet).
    pub fn new(mut world: World, level: Level, game_override: Option<&str>) -> Self {
        let host = std::rc::Rc::new(crate::host::GameHost::new(world.library()));
        let mut g = Game { world, level, host, game_info: None, game_class: "(none)".into(), player_controller: None, accessed_none: 0, accessed_none_by: Default::default(), log_lines: 0, time: 0.0, portal: String::new(), level_serial: 0 };
        g.setup(game_override);
        g
    }

    /// Installs `self.level` into the VM world: actor list, nav network, a fresh host, GameInfo.
    fn setup(&mut self, game_override: Option<&str>) {
        let t0 = std::time::Instant::now();
        self.level_serial += 1;
        let world = &mut self.world;
        let level = &self.level;
        world.vm.missing_native_policy = MissingNativePolicy::Lenient;
        world.vm.level = level.level_info;
        world.vm.actors = level.actors.iter().map(|a| a.1).collect();
        // Navigation network (PathNodes + ReachSpecs) for the AI path-finding natives.
        let nav = p2nav::NavGraph::from_package(&level.map);
        world.vm.set_nav(Box::new(p2nav::VmNav::new(nav, &level.objects)));
        boot_lap(t0, "nav");
        // Engine services (animation, travel, extensions) for this level.
        self.host = std::rc::Rc::new(crate::host::GameHost::new(world.library()));
        self.host.map_lib_insert(&level.map);
        *self.host.map_name.borrow_mut() = level.map.name.clone();
        *self.host.map.borrow_mut() = Some(level.map.clone());
        *self.host.level_objects.borrow_mut() = level.objects.clone();
        world.vm.set_host(self.host.clone());
        crate::host::run_level_hooks(&self.host, &mut world.vm);
        // P2GameInfoSingle.PrepActorsByGroup destroys the map actors whose Group names another
        // day or an inactive holiday, static meshes included (the teen sniper, holiday
        // decorations); the engine lets level-start script remove those bStatic actors.
        world.vm.watch_calls("PrepActorsByGroup", |vm, _, _, after| vm.allow_static_destroy = !after);
        // No date-based holiday content (Valentine's, St Patrick's, Easter, Halloween, Christmas,
        // April Fools): P2GameInfoSingle.IsHoliday returns false when bNoHolidays is set, so the
        // holiday decorations and spawners stay out like the normal game.
        world.vm.watch_calls("IsHoliday", |vm, me, _, after| {
            if let (Some(me), false) = (me, after) {
                vm.set_prop(me, "bNoHolidays", Value::Bool(true));
            }
        });
        boot_lap(t0, "level hooks");
        (self.game_info, self.game_class, self.player_controller, self.time) = (None, "(none)".into(), None, 0.0);
        let gc = Self::game_class(world, level, game_override);
        if let Some(c) = gc {
            self.game_class = world.class_path(c);
            if let Some(o) = world.vm.spawn_actor(c, SpawnParams::default(), false) {
                if let Some(x) = level.xlevel {
                    world.vm.set_prop(o, "XLevel", Value::Object(Some(x)));
                }
                self.game_info = Some(o);
                if let Some(li) = level.level_info {
                    world.vm.set_prop(li, "Game", Value::Object(Some(o)));
                }
            }
        }
        boot_lap(t0, "game info spawned");
    }

    /// Runs the startup sequence described in the module docs.
    pub fn begin_play(&mut self, options: &str) {
        let t0 = std::time::Instant::now();
        let li = self.level.level_info;
        let vm = &mut self.world.vm;
        if let Some(li) = li {
            // Maps are saved mid-session (Greenbelt1: TimeSeconds 76, actors' LastRenderTime up to
            // 76). The clock restarts at 0, so rebase the saved render times onto it: otherwise
            // every pawn looks rendered "in the future", the stasis tick wakes the whole map at
            // load and off-screen animals start fights (FIDELITY #47: the bank dog vs cat brawl).
            let saved = vm.get_prop(li, "TimeSeconds").map(|v| v.as_float()).unwrap_or(0.0);
            if saved > 0.0 {
                for a in vm.actors.clone() {
                    if let Some(Value::Float(t)) = vm.get_prop(a, "LastRenderTime").cloned() {
                        if t != 0.0 {
                            vm.set_prop(a, "LastRenderTime", Value::Float(t - saved));
                        }
                    }
                }
            }
            vm.set_prop(li, "bBegunPlay", Value::Bool(true));
            vm.set_prop(li, "bStartup", Value::Bool(true));
            vm.set_prop(li, "TimeSeconds", Value::Float(0.0));
            vm.set_prop(li, "NetMode", Value::Byte(0));
            if vm.get_prop(li, "TimeDilation").is_none_or(|v| v.as_float() <= 0.0) {
                vm.set_prop(li, "TimeDilation", Value::Float(1.0));
            }
        }
        if let Some(g) = self.game_info {
            if let Err(e) = vm.call_event(g, "InitGame", vec![Value::Str(options.into()), Value::Str(String::new())]) {
                vm.record_error("InitGame", &e);
            }
        }
        boot_lap(t0, "InitGame");
        // Level-saved `Camera` actors are UnrealEd viewports, discarded on map load in game. Left in,
        // StartMatch restarts each as a PlayerController and spawns a stray player pawn for it.
        let cams: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.classes[vm.objects[a].class].name == "Camera").collect();
        for c in cams {
            // Discarded at load, before any script ran: no Destroyed/Logout events (those hit
            // Accessed None in GameInfo.Logout / AdminLogout on every map).
            vm.objects[c].deleted = true;
            vm.set_prop(c, "bDeleteMe", Value::Bool(true));
            vm.actors.retain(|&a| a != c);
        }
        let list: Vec<ObjId> = vm
            .actors
            .clone()
            .into_iter()
            .filter(|&o| !matches!(vm.get_prop(o, "bScriptInitialized"), Some(Value::Bool(true))))
            .collect();
        let timing = std::env::var_os("P2GAME_BOOT_TIMING").is_some();
        let mut by_class: std::collections::HashMap<String, f64> = Default::default();
        let was_profiling = vm.profile;
        if timing {
            vm.profile = true;
            vm.profile_natives.clear();
        }
        for ev in BEGIN_PLAY_ORDER {
            for &o in &list {
                if !vm.objects[o].deleted {
                    let t = std::time::Instant::now();
                    vm.fire_event(o, ev, vec![]);
                    if timing {
                        *by_class.entry(format!("{ev} {}", vm.classes[vm.objects[o].class].name)).or_default() += t.elapsed().as_secs_f64() * 1000.0;
                    }
                }
            }
            boot_lap(t0, ev);
        }
        if timing {
            let mut v: Vec<_> = by_class.into_iter().collect();
            v.sort_by(|a, b| b.1.total_cmp(&a.1));
            for (k, ms) in v.iter().take(12) {
                eprintln!("boot:   {ms:7.1} ms  {k}");
            }
            let mut n: Vec<_> = vm.profile_natives.iter().map(|(k, v)| (k.clone(), *v)).collect();
            n.sort_by(|a, b| b.1 .1.total_cmp(&a.1 .1));
            for (k, (calls, secs)) in n.iter().take(12) {
                eprintln!("boot:   native {:7.1} ms  {calls:6}x  {k}", secs * 1000.0);
            }
            vm.profile = was_profiling;
        }
        for &o in &list {
            vm.set_prop(o, "bScriptInitialized", Value::Bool(true));
        }
        if let Some(li) = li {
            vm.set_prop(li, "bStartup", Value::Bool(false));
        }
        vm.cleanup_destroyed();
        self.login(options);
        boot_lap(t0, "login");
    }

    /// Local player login: `Login(Portal, Options, out Error)` then `PostLogin(NewPlayer, Options)`.
    fn login(&mut self, options: &str) {
        let Some(g) = self.game_info else { return };
        let name = self.world.cfg.player_name.clone();
        let opts = format!("{options}?Name={name}");
        let vm = &mut self.world.vm;
        let pc = vm.fire_event(g, "Login", vec![Value::Str(self.portal.clone()), Value::Str(opts.clone()), Value::Str(String::new())]);
        match pc.and_then(|v| v.as_obj()) {
            Some(pc) => {
                self.player_controller = Some(pc);
                vm.fire_event(g, "PostLogin", vec![Value::Object(Some(pc)), Value::Str(opts)]);
            }
            None => {
                let cls = vm.get_prop(g, "PlayerControllerClassName").map(|v| v.as_str()).unwrap_or_default();
                let start = vm.call(vm.objects[g].class, Some(g), "FindPlayerStart", vec![Value::Object(None), Value::Byte(255), Value::Str(String::new())]);
                let start = match start {
                    Ok(Value::Object(Some(o))) => vm.object_name(o),
                    Ok(_) => "None".into(),
                    Err(e) => e.to_string(),
                };
                *vm.script_errors.entry(format!("Login: returned no PlayerController (PlayerControllerClassName={cls}, FindPlayerStart={start})")).or_default() += 1
            }
        }
    }

    /// One frame of world time.
    pub fn tick(&mut self, dt: f32) {
        self.time += dt;
        let t0 = std::time::Instant::now();
        // Paused (Level.Pauser set by GameInfo.SetPause: the pause menu, map and load screens): the
        // engine only ticks player controllers; world time, animation, physics and the world's
        // sounds hold (menus and screens run on TimeSecondsAlways, advanced by the front end).
        let paused = self.world.vm.level.and_then(|l| self.world.vm.get_prop(l, "Pauser")).and_then(|v| v.as_obj()).is_some();
        crate::audio::set_world_paused(paused);
        if paused {
            let vm = &mut self.world.vm;
            let pcs: Vec<_> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "PlayerController")).collect();
            for pc in pcs {
                vm.tick_actor(pc, dt);
            }
            crate::subtitles::deliver_pending(vm);
            vm.cleanup_destroyed();
            self.host.times.borrow_mut().script += t0.elapsed().as_secs_f64();
            return;
        }
        self.world.vm.tick_world(dt);
        {
            let mut t = self.host.times.borrow_mut();
            t.script += t0.elapsed().as_secs_f64();
            t.frames += 1;
        }
        // Animation after script, as the engine does: AnimEnd/notifies reach script this frame.
        self.host.tick_anims(&mut self.world.vm, dt);
    }

    /// Average milliseconds per frame by phase since the last call (and resets the counters).
    /// Frame hooks are named by the module that registered them where known.
    pub fn take_frame_times(&self) -> Vec<(String, f64)> {
        let t = std::mem::take(&mut *self.host.times.borrow_mut());
        let n = t.frames.max(1) as f64;
        let names = self.host.frame_hook_names.borrow();
        let mut out = vec![("script".to_string(), t.script * 1000.0 / n), ("anim".to_string(), t.anim * 1000.0 / n)];
        for (i, s) in t.hooks.iter().enumerate() {
            let name = names.get(i).cloned().flatten().unwrap_or_else(|| format!("hook{i}"));
            out.push((name, s * 1000.0 / n));
        }
        out
    }

    /// Counts (and optionally echoes) VM log lines since the last call.
    pub fn drain_log(&mut self, echo: bool) {
        // Test hook: P2GAME_SCRIPT_LOG=1 echoes script Log() lines in every mode (window runs too).
        let echo = echo || std::env::var_os("P2GAME_SCRIPT_LOG").is_some();
        for l in std::mem::take(&mut self.world.vm.log) {
            self.log_lines += 1;
            if l.contains("Accessed None") {
                self.accessed_none += 1;
                *self.accessed_none_by.entry(l).or_default() += 1;
            } else if echo {
                println!("  log: {l}");
            }
        }
    }

    pub fn live_actors(&self) -> usize {
        self.world.vm.live_actor_count()
    }

    /// A travel requested by script (`open X`, `ServerTravel`, the menu's New Game), as
    /// (map, URL options), clearing the request.
    pub fn take_travel(&self) -> Option<(String, String)> {
        let url = self.host.pending_travel.borrow_mut().take()?;
        Some(split_url(&url))
    }

    /// Ends this level and boots `map` with `options` on the same VM (classes stay loaded; the
    /// previous level's objects are dropped). The `?Game=` option picks the GameInfo. Returns
    /// false (keeping the current level) if the map cannot be opened.
    pub fn travel(&mut self, map: &str, options: &str) -> bool {
        // `Map#Portal`: the portal names the arrival point (a teleporter tag); the map is before it.
        let (map, portal) = map.split_once('#').unwrap_or((map, ""));
        self.portal = portal.to_string();
        let stem = std::path::Path::new(map).file_stem().and_then(|s| s.to_str()).unwrap_or(map).to_string();
        let Some(pkg) = self.world.files.path(&stem).and_then(|p| upkg::Package::open(p).ok()) else { return false };
        // The local player's pawn and inventory travel with them (GameState, errand items, money).
        let carry = crate::travel::capture(&self.world.vm, self.player_controller);
        let vm = &mut self.world.vm;
        // Keeps class-default subobjects (created lazily, possibly during the level) valid.
        vm.truncate_objects(self.level.base_objects);
        vm.actors.clear();
        vm.level = None;
        vm.log.clear();
        let pkg = std::sync::Arc::new(pkg);
        crate::physics::prefetch(self.world.library(), pkg.clone());
        self.level = self.world.load_level(pkg);
        let game_opt = url_option(options, "Game");
        self.setup(game_opt.as_deref());
        self.begin_play(options);
        let host = self.host.clone();
        crate::travel::restore(&mut self.world.vm, &host, self.player_controller, carry);
        true
    }
}

/// Splits a travel URL `Map?Opt=1?Opt2` into (map, "?Opt=1?Opt2"). A leading `?` keeps the map
/// empty (options only).
pub fn split_url(url: &str) -> (String, String) {
    let url = url.trim().trim_matches('"');
    match url.find('?') {
        Some(i) => (url[..i].to_string(), url[i..].to_string()),
        None => (url.to_string(), String::new()),
    }
}

/// Value of `?Key=Value` in a URL option string (case-insensitive key).
pub fn url_option(options: &str, key: &str) -> Option<String> {
    options.split('?').find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim().eq_ignore_ascii_case(key).then(|| v.trim().to_string())
    })
}

/// Folds per-instance details (offsets, numbers) out of an error message so equal causes group.
pub fn normalise(m: &str) -> String {
    let mut out = String::with_capacity(m.len());
    let mut last_digit = false;
    for c in m.chars() {
        if c.is_ascii_digit() {
            if !last_digit {
                out.push('#');
            }
            last_digit = true;
        } else {
            out.push(c);
            last_digit = false;
        }
    }
    out
}

/// Aggregated histograms over one or more boots: name -> (count, maps it occurred in).
#[derive(Default)]
pub struct Tally {
    pub natives: BTreeMap<String, (u64, usize)>,
    pub errors: BTreeMap<String, (u64, usize)>,
}

impl Tally {
    pub fn add(&mut self, game: &Game) {
        for (k, n) in &game.world.vm.missing_natives {
            let e = self.natives.entry(k.clone()).or_default();
            e.0 += n;
            e.1 += 1;
        }
        let mut errs: BTreeMap<String, u64> = BTreeMap::new();
        for (k, n) in &game.world.vm.script_errors {
            *errs.entry(normalise(k)).or_default() += n;
        }
        for (k, n) in errs {
            let e = self.errors.entry(k).or_default();
            e.0 += n;
            e.1 += 1;
        }
    }

    pub fn sorted_natives(&self) -> Vec<(&String, &(u64, usize))> {
        let mut v: Vec<_> = self.natives.iter().collect();
        v.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
        v
    }

    pub fn sorted_errors(&self) -> Vec<(&String, &(u64, usize))> {
        let mut v: Vec<_> = self.errors.iter().collect();
        v.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
        v
    }

    /// Writes `natives_needed.tsv` and `script_errors.tsv` (tab-separated, most frequent first).
    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let mut f = std::fs::File::create(dir.join("natives_needed.tsv"))?;
        writeln!(f, "native\tcalls\tmaps")?;
        for (name, (calls, maps)) in self.sorted_natives() {
            writeln!(f, "{name}\t{calls}\t{maps}")?;
        }
        let mut f = std::fs::File::create(dir.join("script_errors.tsv"))?;
        writeln!(f, "error\tcount\tmaps")?;
        for (msg, (n, maps)) in self.sorted_errors() {
            writeln!(f, "{}\t{n}\t{maps}", msg.replace(['\t', '\n'], " "))?;
        }
        Ok(())
    }
}
