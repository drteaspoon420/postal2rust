//! States, state code and latent execution.
//!
//! - Each object has a [`StateFrame`]: the current state (a `State` export of its class chain),
//!   the next statement of state code to run, and the pending latent action.
//! - Function calls on an object resolve through the current state first, then through the
//!   state's super states (`state X extends Y`, or the same-named state of a parent class), then
//!   through the class chain.
//! - `ignores` lists compile two ways, both honoured here: probe functions (Timer, Touch, ...)
//!   clear a bit in the state's ignore mask; other functions get a body-less stub in the state
//!   (no `DEFINED` flag), which calls skip.
//! - `GotoState` calls `EndState` on the old state and `BeginState` on the new one. State code
//!   runs from [`Vm::process_state`] (called by [`Vm::tick_actor`]) until it stops or reaches a
//!   latent function; `Sleep` is timed here, other latent actions are completed by
//!   [`crate::host::Host::latent_tick`].
//!
//! Probe bit numbering was established from the shipped packages by comparing each state's
//! cleared ignore bits with the `ignores` declarations of the same state.

use super::*;
use crate::objects::StateData;

/// Probe function names by bit (empty = unknown / unused).
pub const PROBE_NAMES: [&str; 64] = [
    "Spawned",
    "Destroyed",
    "GainedChild",
    "LostChild",
    "",
    "",
    "Trigger",
    "UnTrigger",
    "Timer",
    "HitWall",
    "Falling",
    "Landed",
    "PhysicsVolumeChange",
    "Touch",
    "UnTouch",
    "Bump",
    "BeginState",
    "EndState",
    "BaseChange",
    "Attach",
    "Detach",
    "ActorEntered",
    "ActorLeaving",
    "",
    "AnimEnd",
    "EndedRotation",
    "InterpolateEnd",
    "EncroachingOn",
    "EncroachedBy",
    "",
    "HeadVolumeChange",
    "PostTouch",
    "PawnEnteredVolume",
    "PawnLeavingVolume",
    "",
    "",
    "Tick",
    "PlayerTick",
    "",
    "",
    "SeePlayer",
    "EnemyNotVisible",
    "HearNoise",
    "UpdateEyeHeight",
    "SeeMonster",
    "",
    "SpecialHandling",
    "BotDesireability",
    "NotifyBump",
    "NotifyPhysicsVolumeChange",
    "",
    "NotifyHeadVolumeChange",
    "NotifyLanded",
    "NotifyHitWall",
    "",
    "",
    "",
    "",
    "",
    "",
    "",
    "",
    "",
    "",
];

/// Probe bit of a function name, if it is a probe function.
pub fn probe_bit(name: &str) -> Option<u32> {
    PROBE_NAMES.iter().position(|p| !p.is_empty() && p.eq_ignore_ascii_case(name)).map(|i| i as u32)
}

/// State flag: the class's `auto` state.
const STATE_AUTO: u32 = 0x2;

/// Per-object execution state.
#[derive(Debug, Clone, Default)]
pub struct StateFrame {
    /// Current state (None = no state).
    pub state: Option<FieldId>,
    /// Next state-code statement: the state that owns the code and a statement index.
    pub code: Option<(FieldId, usize)>,
    /// Latent action the state code is waiting on.
    pub latent: Option<Latent>,
    /// Bumped on every state/label change so running state code notices and stops.
    pub generation: u32,
    /// Probe functions turned off with `Disable()`.
    pub disabled: u64,
}

/// A latent function call waiting to complete (`Sleep`, `FinishAnim`, `MoveTo`, ...).
#[derive(Debug, Clone)]
pub struct Latent {
    pub name: String,
    /// Native index of the declaration (e.g. 256 for Sleep, 261 for FinishAnim).
    pub native: u16,
    /// Evaluated arguments (Void for omitted optionals).
    pub args: Vec<Value>,
    /// Seconds since the action started.
    pub elapsed: f32,
}

impl Vm {
    fn state_data(&self, f: FieldId) -> Option<&StateData> {
        match self.obj_of(f)? {
            ScriptObject::State(s) => Some(s),
            _ => None,
        }
    }

    fn super_state(&mut self, s: FieldId) -> Option<FieldId> {
        let sup = self.state_data(s)?.base.super_;
        let f = self.resolve(s.pkg as usize, sup)?;
        self.state_data(f).map(|_| f)
    }

    /// Finds a state by name in a class or its supers (most derived first).
    pub fn find_state(&mut self, class: ClassId, name: &str) -> Option<FieldId> {
        let key = (class, name_hash(name));
        if let Some(r) = self.state_by_name.get(&key) {
            return *r;
        }
        let mut c = Some(class);
        let mut found = None;
        while let Some(ci) = c {
            let f = self.classes[ci].field;
            let sp = &self.pkgs[f.pkg as usize];
            if let Some(k) = sp.children(f.export as usize).into_iter().find(|&k| {
                matches!(sp.objects[k], Some(ScriptObject::State(_))) && sp.export_name(k).eq_ignore_ascii_case(name)
            }) {
                found = Some(FieldId { pkg: f.pkg, export: k as u32 });
                break;
            }
            c = self.classes[ci].super_;
        }
        self.state_by_name.insert(key, found);
        found
    }

    /// The `auto state` of a class (most derived first).
    pub fn auto_state(&mut self, class: ClassId) -> Option<FieldId> {
        let mut c = Some(class);
        while let Some(ci) = c {
            let f = self.classes[ci].field;
            let sp = self.pkgs[f.pkg as usize].clone();
            for k in sp.children(f.export as usize) {
                if let Some(ScriptObject::State(s)) = &sp.objects[k] {
                    if s.state_flags & STATE_AUTO != 0 {
                        // A subclass may override the auto state by name.
                        return self.find_state(class, sp.export_name(k));
                    }
                }
            }
            c = self.classes[ci].super_;
        }
        None
    }

    /// Resolves a virtual call: the object's current state chain first, then the class chain.
    pub fn find_virtual(&mut self, this: Option<ObjId>, class: ClassId, name: &str) -> Option<FieldId> {
        if let Some(o) = this {
            if let Some(st) = self.objects[o].exec.state {
                if let Some(f) = self.find_in_state(st, name) {
                    return Some(f);
                }
            }
        }
        self.find_function(class, name)
    }

    fn find_in_state(&mut self, st: FieldId, name: &str) -> Option<FieldId> {
        let key = (st, name_hash(name));
        if let Some(r) = self.state_lookup.get(&key) {
            return *r;
        }
        let mut s = Some(st);
        let mut found = None;
        let mut depth = 0;
        while let Some(sf) = s {
            let sp = &self.pkgs[sf.pkg as usize];
            if let Some(k) = sp.children(sf.export as usize).into_iter().find(|&k| {
                matches!(sp.objects[k], Some(ScriptObject::Function(_))) && sp.export_name(k).eq_ignore_ascii_case(name)
            }) {
                found = Some(FieldId { pkg: sf.pkg, export: k as u32 });
                break;
            }
            depth += 1;
            s = if depth > 32 { None } else { self.super_state(sf) };
        }
        self.state_lookup.insert(key, found);
        found
    }

    /// True if a probe function is ignored in the object's current state or disabled.
    pub fn is_ignored(&mut self, obj: ObjId, name: &str) -> bool {
        match probe_bit(name) {
            Some(bit) => self.is_ignored_bit(obj, bit),
            None => false,
        }
    }

    pub(crate) fn is_ignored_bit(&self, obj: ObjId, bit: u32) -> bool {
        let ex = &self.objects[obj].exec;
        if ex.disabled & (1u64 << bit) != 0 {
            return true;
        }
        match ex.state.and_then(|s| self.state_data(s)) {
            Some(sd) => sd.ignore_mask & (1u64 << bit) == 0,
            None => false,
        }
    }

    /// `Enable` / `Disable` of a probe function.
    pub fn set_probe_enabled(&mut self, obj: ObjId, name: &str, enabled: bool) {
        if let Some(bit) = probe_bit(name) {
            let d = &mut self.objects[obj].exec.disabled;
            if enabled {
                *d &= !(1u64 << bit);
            } else {
                *d |= 1u64 << bit;
            }
        }
    }

    /// Name of the object's current state (`None` when not in a state).
    pub fn state_name(&self, obj: ObjId) -> String {
        match self.objects[obj].exec.state {
            Some(s) => self.field_name(s),
            None => "None".into(),
        }
    }

    /// True if the current state, or one it extends, has this name.
    pub fn is_in_state(&mut self, obj: ObjId, name: &str) -> bool {
        let mut s = self.objects[obj].exec.state;
        let mut depth = 0;
        while let Some(sf) = s {
            if self.field_name(sf).eq_ignore_ascii_case(name) {
                return true;
            }
            depth += 1;
            s = if depth > 32 { None } else { self.super_state(sf) };
        }
        false
    }

    /// Moves state code to a label of the current state chain. Returns false (and stops state
    /// code) when no state in the chain has the label.
    pub(crate) fn goto_label(&mut self, obj: ObjId, label: &str) -> bool {
        let mut s = self.objects[obj].exec.state;
        let mut found = None;
        let mut depth = 0;
        while let Some(sf) = s {
            let sp = self.pkgs[sf.pkg as usize].clone();
            if let Some(sd) = self.state_data(sf) {
                let script = &sd.base.script;
                for st in &script.stmts {
                    if let Expr::LabelTable(labels) = &st.expr {
                        if let Some(&(_, off)) = labels.iter().find(|(n, _)| sp.pkg.name(*n as usize).eq_ignore_ascii_case(label)) {
                            found = script.stmt_at(off).map(|i| (sf, i));
                        }
                    }
                }
            }
            if found.is_some() {
                break;
            }
            depth += 1;
            s = if depth > 32 { None } else { self.super_state(sf) };
        }
        let ex = &mut self.objects[obj].exec;
        ex.generation = ex.generation.wrapping_add(1);
        ex.latent = None;
        ex.code = found;
        found.is_some()
    }

    /// `GotoState(NewState, Label)`. `None`/empty leaves state code (no state); `Auto` selects the
    /// class's auto state. Calls `EndState`/`BeginState` when the state changes. Returns false if
    /// the named state does not exist (the object stays where it was).
    pub fn goto_state(&mut self, obj: ObjId, new_state: &str, label: &str) -> Result<bool> {
        let class = self.objects[obj].class;
        let none = new_state.is_empty() || new_state.eq_ignore_ascii_case("None");
        let target = if none {
            None
        } else if new_state.eq_ignore_ascii_case("Auto") {
            self.auto_state(class).or_else(|| self.find_state(class, new_state))
        } else {
            match self.find_state(class, new_state) {
                Some(s) => Some(s),
                None => {
                    let msg = format!("Warning: {} GotoState: state {new_state} not found", self.object_name(obj));
                    self.push_log(msg);
                    return Ok(false);
                }
            }
        };
        let label = if label.is_empty() || label.eq_ignore_ascii_case("None") { "Begin" } else { label };
        let cur = self.objects[obj].exec.state;
        if target == cur {
            if target.is_some() {
                self.goto_label(obj, label);
            }
            return Ok(true);
        }
        if cur.is_some() {
            self.call_event(obj, "EndState", vec![])?;
            // EndState may itself change state; that change wins.
            if self.objects[obj].exec.state != cur {
                return Ok(true);
            }
        }
        {
            let ex = &mut self.objects[obj].exec;
            ex.state = target;
            ex.code = None;
            ex.latent = None;
            ex.generation = ex.generation.wrapping_add(1);
        }
        if target.is_some() {
            self.goto_label(obj, label);
            self.call_event(obj, "BeginState", vec![])?;
        }
        Ok(true)
    }

    /// Calls an event/function on an object, resolving through its current state. Returns
    /// `Ok(None)` when the function does not exist, is ignored in the current state (or disabled),
    /// or has no body; script errors are returned.
    pub fn call_event(&mut self, obj: ObjId, name: &str, args: Vec<Value>) -> Result<Option<Value>> {
        if self.objects[obj].deleted && !name.eq_ignore_ascii_case("Destroyed") {
            return Ok(None);
        }
        let class = self.objects[obj].class;
        let Some(f) = self.find_virtual(Some(obj), class, name) else { return Ok(None) };
        let fi = self.func_info(f)?;
        if let Some(bit) = fi.probe {
            if self.is_ignored_bit(obj, bit) {
                return Ok(None);
            }
        }
        if fi.flags & (func_flags::DEFINED | func_flags::NATIVE) == 0 || fi.empty {
            return Ok(None);
        }
        if self.frames.is_empty() {
            self.steps = 0;
        }
        // Events discard the final parameter values, so do not collect them.
        Ok(Some(self.invoke_with(f, Some(obj), Some(class), args, false)?.0))
    }

    /// Like [`Vm::call_event`], also returning the final parameter values (out parameters, e.g.
    /// `PlayerCalcView(out ViewActor, out CameraLocation, out CameraRotation)`).
    pub fn call_event_out(&mut self, obj: ObjId, name: &str, args: Vec<Value>) -> Result<Option<(Value, Vec<Value>)>> {
        if self.objects[obj].deleted {
            return Ok(None);
        }
        let class = self.objects[obj].class;
        let Some(f) = self.find_virtual(Some(obj), class, name) else { return Ok(None) };
        let fi = self.func_info(f)?;
        if let Some(bit) = fi.probe {
            if self.is_ignored_bit(obj, bit) {
                return Ok(None);
            }
        }
        if fi.flags & (func_flags::DEFINED | func_flags::NATIVE) == 0 || fi.empty {
            return Ok(None);
        }
        if self.frames.is_empty() {
            self.steps = 0;
        }
        Ok(Some(self.invoke(f, Some(obj), Some(class), args)?))
    }

    /// The current script call stack, innermost last, as `Object.Owner.Function` strings
    /// (state code frames show as `Object.<state code>`). For diagnostics.
    pub fn backtrace(&self) -> Vec<String> {
        self.frames
            .iter()
            .map(|f| {
                let who = f.this.map(|o| self.object_name(o)).unwrap_or_else(|| "static".into());
                match &f.fi {
                    Some(fi) => format!("{who}.{}.{}", fi.owner, fi.name),
                    None => format!("{who}.<state code>"),
                }
            })
            .collect()
    }

    /// Parses console text into arguments for `obj`'s function `name` the way UE2's exec parser
    /// does: each word converts to the matching parameter's type (`SwitchWeapon 2` gives byte 2),
    /// and a string last parameter takes the rest of the line. None if there is no such function.
    pub fn exec_args(&mut self, obj: ObjId, name: &str, text: &str) -> Option<Vec<Value>> {
        let class = self.objects[obj].class;
        let f = self.find_virtual(Some(obj), class, name)?;
        let fi = self.func_info(f).ok()?;
        let words: Vec<&str> = text.split_whitespace().collect();
        let mut args = Vec::new();
        for (k, &li) in fi.params.iter().enumerate() {
            let Some(w) = words.get(k) else { break };
            let ty = fi.locals[li].2.clone();
            let s = if k + 1 == fi.params.len() && matches!(ty, Ty::Str) { words[k..].join(" ") } else { (*w).to_string() };
            let v = match &ty {
                Ty::Byte => Value::Byte(s.parse::<i64>().unwrap_or(0).clamp(0, 255) as u8),
                Ty::Int => Value::Int(s.parse::<f64>().map(|x| x as i32).unwrap_or(0)),
                Ty::Float => Value::Float(s.parse().unwrap_or(0.0)),
                Ty::Bool => Value::Bool(matches!(s.to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on")),
                Ty::Name => Value::Name(s),
                Ty::Str => Value::Str(s),
                t => crate::config::parse_value(self, t, &s).unwrap_or(Value::Void),
            };
            args.push(v);
        }
        Some(args)
    }

    /// Like [`Vm::call_event`] but script errors are counted in [`Vm::script_errors`] and logged
    /// instead of returned (for engine-fired events).
    pub fn fire_event(&mut self, obj: ObjId, name: &str, args: Vec<Value>) -> Option<Value> {
        let t0 = self.profile.then(std::time::Instant::now);
        let r = match self.call_event(obj, name, args) {
            Ok(v) => v,
            Err(e) => {
                self.record_error(name, &e);
                None
            }
        };
        if let Some(t0) = t0 {
            let c = self.objects[obj].class;
            let e = self.profile_events.entry(format!("{}.{name}", self.classes[c].name)).or_default();
            e.0 += 1;
            e.1 += t0.elapsed().as_secs_f64();
        }
        r
    }

    /// Counts a script error (keyed by message) and logs it with the event that raised it.
    pub fn record_error(&mut self, context: &str, e: &ScriptError) {
        let msg = e.to_string();
        self.push_log(format!("ScriptError: {context}: {msg}"));
        *self.script_errors.entry(msg).or_default() += 1;
    }

    /// Advances the pending latent action by `dt` and, once it completes (or when there is none),
    /// runs state code until it finishes or starts another latent action.
    pub fn process_state(&mut self, obj: ObjId, dt: f32) -> Result<()> {
        if self.objects[obj].deleted {
            return Ok(());
        }
        if let Some(mut l) = self.objects[obj].exec.latent.take() {
            let epoch = self.objects[obj].exec.generation;
            l.elapsed += dt;
            let done = if l.name.eq_ignore_ascii_case("Sleep") {
                let total = l.args.first().map(|v| v.as_float()).unwrap_or(0.0);
                // Scripts read the remaining sleep time from LatentFloat.
                self.set_prop(obj, "LatentFloat", Value::Float((total - l.elapsed).max(0.0)));
                l.elapsed >= total
            } else {
                let h = self.host.clone();
                h.latent_tick(self, obj, &mut l, dt)?
            };
            if self.objects[obj].exec.generation != epoch {
                // The host changed state while completing the action.
            } else if !done {
                self.objects[obj].exec.latent = Some(l);
                return Ok(());
            }
        }
        self.run_state_code(obj)
    }

    /// Runs state code from its current position (no-op while a latent action is pending).
    pub fn run_state_code(&mut self, obj: ObjId) -> Result<()> {
        for _ in 0..16 {
            if self.objects[obj].deleted || self.objects[obj].exec.latent.is_some() {
                return Ok(());
            }
            let Some((sf, pc)) = self.objects[obj].exec.code else { return Ok(()) };
            let epoch = self.objects[obj].exec.generation;
            let sp = self.pkgs[sf.pkg as usize].clone();
            let Some(ScriptObject::State(sd)) = sp.objects.get(sf.export as usize).and_then(|o| o.as_ref()) else {
                self.objects[obj].exec.code = None;
                return Ok(());
            };
            // Formatted only if the state code raises an error (see `Vm::exec`).
            let what = || format!("state {} code", sp.export_name(sf.export as usize));
            if self.frames.is_empty() {
                self.steps = 0;
            }
            let class = self.objects[obj].class;
            self.frames.push(Frame { pkg: sf.pkg as usize, this: Some(obj), class: Some(class), locals: Vec::new(), fi: None, iters: Vec::new() });
            let r = self.exec(&sd.base.script, pc, Mode::State { obj, epoch }, &what);
            self.frames.pop();
            let same = self.objects[obj].exec.generation == epoch;
            match r {
                Err(e) => {
                    if same {
                        self.objects[obj].exec.code = None;
                    }
                    return Err(e);
                }
                Ok(Flow::Latent(next)) => {
                    if same {
                        self.objects[obj].exec.code = Some((sf, next));
                    }
                    return Ok(());
                }
                Ok(Flow::Switched) => continue,
                Ok(_) => {
                    if same {
                        self.objects[obj].exec.code = None;
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    /// One frame for one actor: `Tick` event, latent action + state code, `Timer`, `LifeSpan`.
    /// Script errors are recorded in [`Vm::script_errors`].
    pub fn tick_actor(&mut self, obj: ObjId, dt: f32) {
        if self.objects[obj].deleted {
            return;
        }
        let ids = self.tick_ids();
        // Perf: most actors (StaticMeshActor, Brush, PathNode...) have no Tick handler. Outside
        // a state the lookup depends only on the class, so cache "nothing to call" per class and
        // skip the event (lookup, FuncInfo clone and argument Vec) for them.
        let class = self.objects[obj].class;
        let stateless = self.objects[obj].exec.state.is_none();
        let noop = stateless && self.tick_noop.get(class).copied().flatten().unwrap_or_else(|| {
            let n = match self.find_function(class, "Tick") {
                None => true,
                Some(f) => self.func_info(f).map(|fi| fi.flags & (crate::vm::func_flags::DEFINED | crate::vm::func_flags::NATIVE) == 0 || fi.empty).unwrap_or(false),
            };
            if self.tick_noop.len() <= class {
                self.tick_noop.resize(class + 1, None);
            }
            self.tick_noop[class] = Some(n);
            n
        });
        if !noop {
            self.fire_event(obj, "Tick", vec![Value::Float(dt)]);
        }
        if self.objects[obj].exec.code.is_some() || self.objects[obj].exec.latent.is_some() {
            if let Err(e) = self.process_state(obj, dt) {
                let ctx = format!("{} state code", self.state_name(obj));
                self.record_error(&ctx, &e);
            }
        }
        if self.objects[obj].deleted {
            return;
        }
        let Some([rate_s, counter_s, loop_s, life_s]) = ids.get(self.objects[obj].class).copied().flatten() else { return };
        let rate = self.objects[obj].slots[rate_s].as_float();
        if rate > 0.0 {
            let counter = self.objects[obj].slots[counter_s].as_float() + dt;
            if counter >= rate {
                let looping = self.objects[obj].slots[loop_s].as_bool();
                if looping {
                    self.objects[obj].slots[counter_s] = Value::Float(counter - rate * (counter / rate).floor());
                } else {
                    self.objects[obj].slots[rate_s] = Value::Float(0.0);
                    self.objects[obj].slots[counter_s] = Value::Float(0.0);
                }
                self.fire_event(obj, "Timer", vec![]);
            } else {
                self.objects[obj].slots[counter_s] = Value::Float(counter);
            }
        }
        if self.objects[obj].deleted {
            return;
        }
        let life = self.objects[obj].slots[life_s].as_float();
        if life > 0.0 {
            let left = life - dt;
            if left <= 0.0001 {
                self.objects[obj].slots[life_s] = Value::Float(0.0);
                self.destroy_actor(obj);
            } else {
                self.objects[obj].slots[life_s] = Value::Float(left);
            }
        }
    }

    /// Slot indices of TimerRate, TimerCounter, bTimerLoop and LifeSpan per class (None for
    /// non-actors), extended as classes load.
    fn tick_ids(&mut self) -> std::rc::Rc<Vec<Option<[usize; 4]>>> {
        if self.tick_slots.len() != self.classes.len() {
            let v: Vec<Option<[usize; 4]>> = self
                .classes
                .iter()
                .map(|c| Some([c.slot("TimerRate")?, c.slot("TimerCounter")?, c.slot("bTimerLoop")?, c.slot("LifeSpan")?]))
                .collect();
            self.tick_slots = std::rc::Rc::new(v);
        }
        self.tick_slots.clone()
    }

    /// One world frame: advances `Level.TimeSeconds`, ticks every live actor in spawn order and
    /// then clears references to actors destroyed during the frame.
    pub fn tick_world(&mut self, dt: f32) {
        if let Some(l) = self.level {
            let dil = self.get_prop(l, "TimeDilation").map(|v| v.as_float()).filter(|d| *d > 0.0).unwrap_or(1.0);
            let t = self.get_prop(l, "TimeSeconds").map(|v| v.as_float()).unwrap_or(0.0);
            self.set_prop(l, "TimeSeconds", Value::Float(t + dt * dil));
        }
        let list = self.actors.clone();
        for a in list {
            self.tick_actor(a, dt);
        }
        self.cleanup_destroyed();
    }
}
