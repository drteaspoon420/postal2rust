//! Movies and Matinee cutscenes for the game host (p2movie, p2matinee).
//!
//! [`install`] registers a level hook that pushes a [`CinemaHost`] into [`GameHost::ext`] and a
//! frame hook:
//!
//! **Movies.** `HUD.Movie` and `MovieTexture.Movie` are transient `Movie` objects the engine
//! creates natively. When a HUD or MovieTexture asks for them (`InitializeMovie`, or any `Movie`
//! native called through a `None` object) a VM object of class `Engine.Movie` is created and stored
//! in the property, and its natives (`Play`, `Pause`, `IsPaused`, `StopNow`, `StopAtEnd`,
//! `IsPlaying`, `GetWidth`, `GetHeight`) drive one shared [`MoviePlayer`] (the game shows one movie
//! at a time). The front end reads [`Cinema::movie`] to draw it; headless runs just advance it.
//!
//! **Matinee.** Every `SceneManager` of the level gets a [`ScenePlayer`] built from the map's
//! decoded scene. Script's `Trigger` on a SceneManager sets `bIsRunning`; the frame hook notices
//! and starts the player. `GetTotalSceneTime` returns the runtime's total. Each frame the player
//! ticks and its [`ViewState`] goes to the viewer (`SceneManager.Viewer`, else the local
//! PlayerController): Location/Rotation of the viewer, `FOVAngle`/`DesiredFOV`, the fade
//! (`FlashScale`/`FlashFog`-style screen fade exposed as [`Cinema::fade`]), letterbox
//! (`myHUD.bCinematicView`), `CurrentTime`/`PctSceneComplete`/`SceneSpeed` on the SceneManager.
//! Events: `SceneStarted` / `SceneEnded` script events, `TriggerEvent` for SubActionTrigger and
//! `NextSceneTag`. The audio listener follows the camera while a scene runs.

use crate::host::GameHost;
use glam::Vec3;
use p2matinee::{LookTarget, SceneEvent, SceneHost, ScenePlayer, ViewState, scenes_in};
use p2movie::MoviePlayer;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use uscript::host::{Host, NativeCall};
use uscript::vm::{ObjId, Value, Vm};

pub struct Cinema {
    pub game_dir: PathBuf,
    /// The movie being shown (HUD or MovieTexture).
    pub movie: RefCell<MoviePlayer>,
    /// VM objects of class Movie created for HUD / MovieTexture.
    movie_objects: RefCell<Vec<ObjId>>,
    /// MovieTexture objects created for DynamicLoadObject, by lower-case path.
    movie_textures: RefCell<HashMap<String, ObjId>>,
    /// SceneManager actor -> runtime.
    scenes: RefCell<HashMap<ObjId, ScenePlayer>>,
    /// Latest camera state of the running scene (for the front end).
    pub view: RefCell<Option<ViewState>>,
    pub stats: RefCell<CinemaStats>,
}

#[derive(Default, Debug, Clone)]
pub struct CinemaStats {
    pub movies_played: u64,
    pub movie_frames: u64,
    pub scenes: usize,
    pub scenes_started: u64,
    pub scenes_ended: u64,
    pub scene_triggers: u64,
    pub scenes_skipped: u64,
}

thread_local! {
    static CINEMA: RefCell<Option<Rc<Cinema>>> = const { RefCell::new(None) };
}

pub fn cinema() -> Option<Rc<Cinema>> {
    CINEMA.with(|c| c.borrow().clone())
}

pub fn install(game_dir: PathBuf) -> Rc<Cinema> {
    let c = Rc::new(Cinema {
        game_dir,
        movie: RefCell::new(MoviePlayer::default()),
        movie_objects: RefCell::new(Vec::new()),
        movie_textures: RefCell::new(HashMap::new()),
        scenes: RefCell::new(HashMap::new()),
        view: RefCell::new(None),
        stats: RefCell::new(CinemaStats::default()),
    });
    CINEMA.with(|s| *s.borrow_mut() = Some(c.clone()));
    crate::host::register_level_hook(level_hook);
    c
}

fn level_hook(host: &GameHost, vm: &mut Vm) {
    let Some(c) = cinema() else { return };
    c.movie.borrow_mut().stop_now();
    c.movie_objects.borrow_mut().clear();
    c.movie_textures.borrow_mut().clear();
    *c.view.borrow_mut() = None;
    // Decode the level's scenes and pair them with the SceneManager actors (export index first,
    // then object name).
    let mut scenes = HashMap::new();
    set_export_map(&host.level_objects.borrow());
    let pkg = host.map.borrow().clone().or_else(|| host.lib.package(&host.map_name.borrow()));
    if let Some(pkg) = pkg {
        let decoded = scenes_in(&pkg);
        let lo = host.level_objects.borrow();
        for s in &decoded {
            let a = lo.get(&s.export).copied().or_else(|| {
                vm.actors.iter().copied().find(|&a| is_a(vm, a, "SceneManager") && vm.get_prop(a, "Name").is_some_and(|v| v.as_name().eq_ignore_ascii_case(&s.name)))
            });
            if let Some(a) = a {
                scenes.insert(a, ScenePlayer::new(s.clone()));
            }
        }
    }
    c.stats.borrow_mut().scenes = scenes.len();
    *c.scenes.borrow_mut() = scenes;
    host.ext.borrow_mut().push(Rc::new(CinemaHost { c: c.clone() }));
    let c2 = c.clone();
    host.frame_hooks.borrow_mut().push(Box::new(move |vm, dt| c2.frame(vm, dt)));
}

pub fn is_a(vm: &Vm, a: ObjId, class: &str) -> bool {
    let mut c = Some(vm.objects[a].class);
    while let Some(ci) = c {
        if vm.classes[ci].name.eq_ignore_ascii_case(class) {
            return true;
        }
        c = vm.classes[ci].super_;
    }
    false
}

/// Resolves LookAt targets against the live level.
struct Actors<'a> {
    vm: &'a Vm,
    export_to_obj: &'a dyn Fn(usize) -> Option<ObjId>,
    seed: u32,
}

impl SceneHost for Actors<'_> {
    fn actor_location(&mut self, t: LookTarget<'_>) -> Option<Vec3> {
        let a = match t {
            LookTarget::Export(e) => (self.export_to_obj)(e)?,
            LookTarget::Tag(tag) => self.vm.actors.iter().copied().find(|&a| !self.vm.objects[a].deleted && self.vm.get_prop(a, "Tag").is_some_and(|v| v.as_name().eq_ignore_ascii_case(tag)))?,
        };
        Some(Vec3::from(self.vm.actor_loc(a)))
    }
    fn frand(&mut self) -> f32 {
        self.seed = self.seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.seed >> 8) as f32 / (1u32 << 24) as f32
    }
}

impl Cinema {
    /// The screen fade of the running scene: (RGBA color, coverage 0..1).
    pub fn fade(&self) -> Option<([u8; 4], f32)> {
        self.view.borrow().as_ref().and_then(|v| v.fade)
    }

    fn frame(&self, vm: &mut Vm, dt: f32) {
        if self.movie.borrow_mut().tick(dt as f64) {
            self.stats.borrow_mut().movie_frames += 1;
        }
        self.tick_scenes(vm, dt);
    }

    fn tick_scenes(&self, vm: &mut Vm, dt: f32) {
        let ids: Vec<ObjId> = self.scenes.borrow().keys().copied().collect();
        let mut any_view = None;
        for sm in ids {
            if vm.objects[sm].deleted {
                continue;
            }
            // Script's Trigger() sets bIsRunning; start the runtime then.
            let want = vm.get_prop(sm, "bIsRunning").is_some_and(|v| v.as_bool());
            // Skip / fast-forward: the viewer's `bWantsToSkip` input byte, honoured per the
            // SceneManager's current bLetPlayerSkip / bLetPlayerFastForward (script may change
            // them, e.g. P2Player.StartCutscene for bLetPlayerSkipIfSeen).
            let viewer = vm.get_prop(sm, "Viewer").and_then(|x| x.as_obj());
            let wants_skip = viewer.and_then(|v| vm.get_prop(v, "bWantsToSkip")).is_some_and(|v| v.as_int() != 0);
            let can_skip = vm.get_prop(sm, "bLetPlayerSkip").is_some_and(|v| v.as_bool());
            let can_ff = vm.get_prop(sm, "bLetPlayerFastForward").is_some_and(|v| v.as_bool());
            let mut skipped = false;
            let mut events = Vec::new();
            let view = {
                let mut scenes = self.scenes.borrow_mut();
                let Some(p) = scenes.get_mut(&sm) else { continue };
                if want && !p.is_running() {
                    p.trigger();
                }
                if !p.is_running() {
                    continue;
                }
                p.scene.let_player_skip = can_skip;
                p.scene.let_player_fast_forward = can_ff;
                p.fast_forward = wants_skip && can_ff;
                if wants_skip && can_skip && p.time > 0.0 {
                    skipped = p.skip();
                }
                let lookup = |e: usize| export_actor(vm, e);
                let mut host = Actors { vm, export_to_obj: &lookup, seed: sm as u32 ^ 0x9e37 };
                p.tick(dt, &mut host, &mut events)
            };
            if skipped {
                // The key press is consumed by the skip (one press per scene).
                if let Some(v) = viewer {
                    vm.set_prop(v, "bWantsToSkip", Value::Byte(0));
                }
                self.stats.borrow_mut().scenes_skipped += 1;
            }
            for e in events {
                self.scene_event(vm, sm, e);
            }
            if let Some(v) = view {
                // `P2GAME_CINEMA_TRACE=1`: the scene camera about twice a second.
                if std::env::var_os("P2GAME_CINEMA_TRACE").is_some() {
                    let t = self.scenes.borrow().get(&sm).map(|p| p.time).unwrap_or(0.0);
                    if (t * 2.0).fract() < dt * 2.0 {
                        println!("CINEMA {} t={t:.2} cam {:?} rot {:?} fov {:?} fade {:?}", vm.object_name(sm), v.location, v.rotation, v.fov, v.fade);
                    }
                }
                self.apply_view(vm, sm, &v);
                any_view = Some(v);
            }
        }
        crate::audio::set_listener_override(any_view.as_ref().map(|v| (v.location, v.rotation)));
        *self.view.borrow_mut() = any_view;
    }

    fn scene_event(&self, vm: &mut Vm, sm: ObjId, e: SceneEvent) {
        match e {
            SceneEvent::Started => {
                self.stats.borrow_mut().scenes_started += 1;
                vm.set_prop(sm, "bIsSceneStarted", Value::Bool(true));
                let total = self.scenes.borrow().get(&sm).map(|p| p.total_scene_time()).unwrap_or(0.0);
                vm.set_prop(sm, "TotalSceneTime", Value::Float(total));
                vm.fire_event(sm, "SceneStarted", vec![]);
                if std::env::var_os("P2GAME_CINEMA_TRACE").is_some() {
                    let o = |n: &str| vm.get_prop(sm, n).and_then(|v| v.as_obj()).map(|x| vm.object_name(x));
                    println!("CINEMA {} SceneStarted viewer {:?} oldpawn {:?} preceding {:?} affect {:?}", vm.object_name(sm), o("Viewer"), o("OldPawn"), o("PrecedingScenesPawn"), vm.get_prop(sm, "Affect"));
                }
            }
            SceneEvent::Ended => {
                self.stats.borrow_mut().scenes_ended += 1;
                vm.set_prop(sm, "bIsRunning", Value::Bool(false));
                vm.fire_event(sm, "SceneEnded", vec![]);
                if std::env::var_os("P2GAME_CINEMA_TRACE").is_some() {
                    println!("CINEMA {} SceneEnded", vm.object_name(sm));
                }
            }
            SceneEvent::Trigger(ev) | SceneEvent::TriggerNext(ev) => {
                self.stats.borrow_mut().scene_triggers += 1;
                if std::env::var_os("P2GAME_CINEMA_TRACE").is_some() {
                    println!("CINEMA {} TriggerEvent {ev}", vm.object_name(sm));
                }
                let viewer = vm.get_prop(sm, "Viewer").cloned().unwrap_or(Value::Object(None));
                let inst = match viewer.as_obj().and_then(|v| vm.get_prop(v, "Pawn").and_then(|p| p.as_obj())) {
                    Some(p) => Value::Object(Some(p)),
                    None => Value::Object(None),
                };
                vm.fire_event(sm, "TriggerEvent", vec![Value::Name(ev), Value::Object(Some(sm)), inst]);
            }
        }
    }

    /// Writes the camera into the viewer and the SceneManager's transient state.
    fn apply_view(&self, vm: &mut Vm, sm: ObjId, v: &ViewState) {
        let p = self.scenes.borrow().get(&sm).map(|p| (p.time, p.scene_speed)).unwrap_or((0.0, 1.0));
        vm.set_prop(sm, "CurrentTime", Value::Float(p.0));
        vm.set_prop(sm, "SceneSpeed", Value::Float(p.1));
        vm.set_prop(sm, "PctSceneComplete", Value::Float(v.pct));
        let viewer = vm.get_prop(sm, "Viewer").and_then(|x| x.as_obj()).or_else(|| local_player(vm));
        let Some(viewer) = viewer else { return };
        vm.set_prop(viewer, "Location", Value::Vector(v.location.to_array()));
        vm.set_prop(viewer, "Rotation", Value::Rotator(v.rotation));
        if let Some(fov) = v.fov {
            vm.set_prop(viewer, "FOVAngle", Value::Float(fov));
            vm.set_prop(viewer, "DesiredFOV", Value::Float(fov));
        }
        if let Some(hud) = vm.get_prop(viewer, "myHUD").and_then(|x| x.as_obj()) {
            vm.set_prop(hud, "bCinematicView", Value::Bool(v.cinematic));
        }
        if let Some(gs) = v.game_speed {
            if let Some(li) = vm.level {
                vm.set_prop(li, "TimeDilation", Value::Float(gs.max(0.01)));
            }
        }
    }

    fn movie_native(&self, vm: &mut Vm, me: Option<ObjId>, name: &str, a: &[Value]) -> Option<Value> {
        let mut m = self.movie.borrow_mut();
        Some(match name {
            "play" => {
                let file = a.first().map(|v| v.as_str()).unwrap_or_default();
                let looping = a.get(2).is_some_and(|v| v.as_bool());
                // The HUD calls Play on HUD.Movie (direct) or MovieTexture.Movie (scaled).
                let scaled = me.is_some_and(|o| vm.get_prop(o, "Outer").and_then(|x| x.as_obj()).is_some_and(|ow| is_a(vm, ow, "MovieTexture")));
                let r = if scaled { m.play_scaled(&self.game_dir, &file, 0.0, 0.0, 1.0, 1.0, false, looping, false) } else { m.play_direct(&self.game_dir, &file, 0, 0, false, looping) };
                match r {
                    Ok(()) => self.stats.borrow_mut().movies_played += 1,
                    Err(e) => vm.push_log(format!("Movie.Play: {e}")),
                }
                Value::Void
            }
            "pause" => {
                m.pause(a.first().is_some_and(|v| v.as_bool()));
                Value::Void
            }
            "ispaused" => Value::Bool(m.is_paused()),
            "stopnow" => {
                m.stop_now();
                Value::Void
            }
            "stopatend" => {
                m.stop_at_end();
                Value::Void
            }
            "isplaying" => Value::Bool(m.is_playing()),
            "getwidth" => Value::Int(m.width() as i32),
            "getheight" => Value::Int(m.height() as i32),
            _ => return None,
        })
    }

    /// Creates the transient Movie object for a HUD or MovieTexture holder.
    fn ensure_movie_object(&self, vm: &mut Vm, holder: ObjId) -> Option<ObjId> {
        if let Some(o) = vm.get_prop(holder, "Movie").and_then(|v| v.as_obj()) {
            return Some(o);
        }
        let cls = vm.find_class("Engine", "Movie").ok()?;
        let o = vm.new_object(cls);
        vm.set_prop(o, "Outer", Value::Object(Some(holder)));
        vm.set_prop(holder, "Movie", Value::Object(Some(o)));
        self.movie_objects.borrow_mut().push(o);
        Some(o)
    }
}

/// The level actor created from map export `e` (actors keep their export name).
fn export_actor(vm: &Vm, e: usize) -> Option<ObjId> {
    crate::cinema::EXPORT_MAP.with(|m| m.borrow().get(&e).copied()).filter(|&a| !vm.objects[a].deleted)
}

thread_local! {
    /// Map export index -> VM object for the current level (filled by the boot/travel code
    /// through [`set_export_map`]).
    static EXPORT_MAP: RefCell<HashMap<usize, ObjId>> = RefCell::new(HashMap::new());
}

/// Gives the cinema the level's export-to-object table (for `LookAt` actor references).
pub fn set_export_map(m: &HashMap<usize, ObjId>) {
    EXPORT_MAP.with(|x| *x.borrow_mut() = m.clone());
}

fn local_player(vm: &Vm) -> Option<ObjId> {
    crate::ai::local_player_controller(vm)
}

struct CinemaHost {
    c: Rc<Cinema>,
}

impl Host for CinemaHost {
    fn native(&self, vm: &mut Vm, call: &NativeCall, args: &mut Vec<Value>) -> Option<uscript::Result<Value>> {
        let name = call.name.to_ascii_lowercase();
        if call.class.eq_ignore_ascii_case("Movie") {
            return self.c.movie_native(vm, call.this, &name, args).map(Ok);
        }
        match (call.class.to_ascii_lowercase().as_str(), name.as_str()) {
            ("movietexture", "initializemovie") => {
                if let Some(me) = call.this {
                    self.c.ensure_movie_object(vm, me);
                }
                Some(Ok(Value::Void))
            }
            ("scenemanager", "gettotalscenetime") => {
                let t = call.this.and_then(|me| self.c.scenes.borrow().get(&me).map(|p| p.total_scene_time())).unwrap_or(0.0);
                Some(Ok(Value::Float(t)))
            }
            _ => None,
        }
    }

    fn actor_spawned(&self, vm: &mut Vm, actor: ObjId) {
        // HUDs get their Movie object as the engine does at spawn.
        if is_a(vm, actor, "HUD") {
            self.c.ensure_movie_object(vm, actor);
        }
    }

    /// `DynamicLoadObject("MovieTextures.Generic", class'MovieTexture')`: one VM object per path,
    /// with the saved `MovieFilename` / `FrameRate` defaults of the class.
    fn load_object(&self, vm: &mut Vm, path: &str, class: uscript::vm::ClassId) -> Option<Value> {
        let mt = vm.find_class("Engine", "MovieTexture").ok()?;
        if !vm.is_child_of(class, mt) {
            return None;
        }
        let key = path.to_ascii_lowercase();
        if let Some(&o) = self.c.movie_textures.borrow().get(&key) {
            return Some(Value::Object(Some(o)));
        }
        let o = vm.new_object(class);
        let leaf = path.rsplit('.').next().unwrap_or(path).to_string();
        vm.set_prop(o, "Name", Value::Name(leaf));
        self.c.movie_textures.borrow_mut().insert(key, o);
        Some(Value::Object(Some(o)))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn export_map_roundtrip() {
        let mut m = std::collections::HashMap::new();
        m.insert(3usize, 7usize);
        super::set_export_map(&m);
        super::EXPORT_MAP.with(|x| assert_eq!(x.borrow().get(&3), Some(&7)));
    }
}
