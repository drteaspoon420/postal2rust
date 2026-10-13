//! Native half of the subtitle system (`Engine.SubtitleManager`, config `Subtitles.ini`).
//!
//! Script owns the data and the drawing: the player controller finds or spawns the
//! `SubtitleManager` (its config arrays come from `Subtitles.ini`), `P2Player.CallSubtitle`
//! picks text, colours and speaker name and hands them to `P2HUD.AddSubtitles`. What the engine
//! adds is the hook in sound playback: when a sound listed in `SubSound` plays, it calls the
//! local player's `CallSubtitle(Sound, index, ActorIndex, Actor)`, where `ActorIndex` is the
//! position of the speaker's `ActorID` in the manager's `ActorID` list (or -1).
//!
//! Sounds are queued while they play (inside script natives) and delivered once per frame, as the
//! manager's own `PendingSounds` list does.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use uscript::host::SoundRequest;
use uscript::vm::{ObjId, Value, Vm};

#[derive(Default)]
struct State {
    /// Manager object the index tables were built from.
    manager: Option<ObjId>,
    /// Lower-case sound path -> index in `SubSound`.
    sounds: HashMap<String, usize>,
    /// Lower-case `ActorID` -> index.
    actor_ids: HashMap<String, usize>,
    pending: Vec<(String, usize, Option<ObjId>)>,
}

struct SubtitleHost {
    st: Rc<RefCell<State>>,
}

fn manager(vm: &Vm) -> Option<(ObjId, ObjId)> {
    let pc = crate::ai::local_player_controller(vm)?;
    let m = vm.get_prop(pc, "SubtitleManager")?.as_obj()?;
    (!vm.objects[m].deleted).then_some((pc, m))
}

fn key(s: &str) -> String {
    s.trim().trim_matches('\'').to_ascii_lowercase()
}

impl State {
    fn rebuild(&mut self, vm: &Vm, m: ObjId) {
        self.manager = Some(m);
        self.sounds.clear();
        self.actor_ids.clear();
        if let Some(Value::Array(a)) = vm.get_prop(m, "SubSound") {
            for (i, v) in a.iter().enumerate() {
                if let Some(p) = v.as_resource() {
                    self.sounds.entry(key(p)).or_insert(i);
                }
            }
        }
        if let Some(Value::Array(a)) = vm.get_prop(m, "ActorID") {
            for (i, v) in a.iter().enumerate() {
                let n = v.as_name();
                if !n.is_empty() && !v.is_none_name() {
                    self.actor_ids.entry(n.to_ascii_lowercase()).or_insert(i);
                }
            }
        }
    }
}

impl uscript::host::Host for SubtitleHost {
    fn play_sound(&self, vm: &mut Vm, actor: Option<ObjId>, req: &SoundRequest) {
        let Some((_, m)) = manager(vm) else { return };
        let mut st = self.st.borrow_mut();
        if st.manager != Some(m) {
            st.rebuild(vm, m);
        }
        if let Some(&i) = st.sounds.get(&key(&req.sound)) {
            if st.pending.len() < 64 {
                st.pending.push((req.sound.clone(), i, actor));
            }
        }
    }
}

fn deliver(st: &Rc<RefCell<State>>, vm: &mut Vm) {
    let pending = std::mem::take(&mut st.borrow_mut().pending);
    if pending.is_empty() {
        return;
    }
    let Some((pc, _)) = manager(vm) else { return };
    for (sound, index, actor) in pending {
        let actor = actor.filter(|&a| !vm.objects[a].deleted);
        let aindex = actor
            .and_then(|a| vm.get_prop(a, "ActorID").map(|v| v.as_name()))
            .and_then(|n| st.borrow().actor_ids.get(&n.to_ascii_lowercase()).copied())
            .map_or(-1, |i| i as i32);
        // `P2GAME_SUBS=1` traces which sounds raise a subtitle.
        if std::env::var_os("P2GAME_SUBS").is_some() {
            println!("SUBS {sound} index {index} actor index {aindex}");
        }
        let args = vec![Value::Resource(sound), Value::Int(index as i32), Value::Int(aindex), Value::Object(actor)];
        if let Err(e) = vm.call_event(pc, "CallSubtitle", args) {
            vm.record_error("CallSubtitle", &e);
        }
    }
}

fn level_hook(host: &crate::host::GameHost, _vm: &mut Vm) {
    let st = Rc::new(RefCell::new(State::default()));
    host.ext.borrow_mut().push(Rc::new(SubtitleHost { st: st.clone() }));
    CURRENT.with(|c| *c.borrow_mut() = Some(st.clone()));
    host.add_frame_hook("subtitles", move |vm, _dt| deliver(&st, vm));
}

thread_local! {
    static CURRENT: RefCell<Option<Rc<RefCell<State>>>> = const { RefCell::new(None) };
}

/// Delivers queued subtitles now. Frame hooks do not run while the game is paused, but screens
/// that pause it (the map screen's errand dialogue) still raise subtitles, so the paused tick
/// calls this.
pub fn deliver_pending(vm: &mut Vm) {
    if let Some(st) = CURRENT.with(|c| c.borrow().clone()) {
        deliver(&st, vm);
    }
}

/// Registers the subtitle hook for every level. Call before the first level boots.
pub fn install() {
    crate::host::register_level_hook(level_hook);
}
