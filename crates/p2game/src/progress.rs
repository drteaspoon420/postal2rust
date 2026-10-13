//! `p2game --progress`: headless whole-game progression check. Boots the first day map as a new
//! single-player game and, for each day, does what a player does at the end of the day: the
//! day's errands are completed through the script's own errand completion (the debug path
//! `SetTodaysErrandsComplete`, with the debug menu forced on as the shipped game's cheat does),
//! the player goes home (`AtPlayerHouse`), the end-of-day map runs its sequence (skippable
//! scenes skipped with `bWantsToSkip`, screens dismissed with their end key), and the next day
//! starts. Per day it records maps visited, the errand list and completion, new script errors
//! and missing natives, and writes `<out>/progress.tsv`.
//!
//! The day order and URLs come from the GameInfo (`Days[]`, `FinishedDayURL`,
//! `StartNextDayURL`), so whatever the install routes to (weekend days included) is followed.

use crate::frontend::{FrameInput, Frontend};
use p2game::boot::Game;
use p2input::bevy_map::InputEvent;
use p2input::{Action, Key};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use uscript::vm::{ObjId, Value, Vm};

#[derive(Default, Debug)]
struct DayReport {
    day: i32,
    name: String,
    maps: Vec<String>,
    errands: Vec<(String, bool)>,
    new_errors: BTreeMap<String, u64>,
    new_natives: BTreeMap<String, u64>,
    ended: bool,
    notes: Vec<String>,
    frames: u32,
}

fn game_state(vm: &Vm, game: ObjId) -> Option<ObjId> {
    vm.get_prop(game, "TheGameState").and_then(|v| v.as_obj()).filter(|&o| o < vm.objects.len() && !vm.objects[o].deleted)
}

fn current_day(vm: &Vm, game: Option<ObjId>) -> Option<i32> {
    let gs = game_state(vm, game?)?;
    vm.get_prop(gs, "CurrentDay").map(|v| v.as_int())
}

fn days(vm: &Vm, game: ObjId) -> Vec<ObjId> {
    match vm.get_prop(game, "Days") {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_obj()).collect(),
        _ => Vec::new(),
    }
}

/// Day index and (errand, completed) list of the running game, for test probes.
pub(crate) fn errand_state(vm: &Vm) -> Option<(i32, Vec<(String, bool)>)> {
    let g = vm.level.and_then(|l| vm.get_prop(l, "Game")).and_then(|v| v.as_obj())?;
    let day = current_day(vm, Some(g))?;
    Some((day, errands(vm, g, day)))
}

fn errands(vm: &Vm, game: ObjId, day: i32) -> Vec<(String, bool)> {
    let Some(&d) = days(vm, game).get(day.max(0) as usize) else { return Vec::new() };
    let gs = game_state(vm, game);
    let done: Vec<String> = gs
        .and_then(|g| match vm.get_prop(g, "CompletedErrands") {
            Some(Value::Array(a)) => Some(a.iter().map(|v| vm.value_string(v)).collect()),
            _ => None,
        })
        .unwrap_or_default();
    match vm.get_prop(d, "Errands") {
        Some(Value::Array(es)) => es
            .iter()
            .filter_map(|e| e.as_obj())
            .map(|e| {
                let name = vm.get_prop(e, "UniqueName").map(|v| v.as_str()).unwrap_or_default();
                let complete = vm.get_prop(e, "bCompleted").is_some_and(|v| v.as_bool()) || done.iter().any(|d| d.contains(&name));
                (name, complete)
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn call(vm: &mut Vm, o: ObjId, f: &str, args: Vec<Value>) {
    if let Err(e) = vm.call_event(o, f, args) {
        vm.record_error(&format!("progress {f}"), &e);
    }
}

fn counts(m: &std::collections::HashMap<String, u64>) -> BTreeMap<String, u64> {
    m.iter().map(|(k, v)| (k.clone(), *v)).collect()
}

fn diff(now: &BTreeMap<String, u64>, before: &BTreeMap<String, u64>) -> BTreeMap<String, u64> {
    now.iter()
        .filter_map(|(k, v)| {
            let d = v.saturating_sub(*before.get(k).unwrap_or(&0));
            (d > 0).then(|| (k.clone(), d))
        })
        .collect()
}

/// The running P2Screen (map/news/load screen), if any.
fn current_screen(vm: &Vm, pc: ObjId) -> Option<ObjId> {
    vm.get_prop(pc, "CurrentScreen").and_then(|v| v.as_obj()).filter(|&s| vm.get_prop(s, "bIsRunning").is_some_and(|v| v.as_bool()))
}

pub(crate) fn run(mut game: Game, game_root: &std::path::Path, out_dir: &std::path::Path, max_frames: u32, last_day: Option<i32>) -> i32 {
    let (w, h) = (1024, 768);
    let mut front = Frontend::install(&mut game, game_root, w, h, true);
    let dt = 1.0 / 30.0;
    let t0 = std::time::Instant::now();
    let mut reports: Vec<DayReport> = Vec::new();
    let mut cur = DayReport { day: -1, ..Default::default() };
    let mut errs0 = counts(&game.world.vm.script_errors);
    let mut nat0 = counts(&game.world.vm.missing_natives);
    let mut frames_on_map = 0u32;
    let mut phase_frames = 0u32;
    // Phase: 0 settle on the day map, 1 errands completed and sent home, 2 at home / end of day.
    let mut phase = 0;
    let mut events: Vec<InputEvent> = Vec::new();
    let mut quit = false;
    let mut game_over = false;
    for frame in 0..max_frames {
        game.tick(dt);
        game.drain_log(false);
        let _ = p2game::save::process_requests(&mut game);
        let evs = std::mem::take(&mut events);
        let out = front.frame(&mut game, FrameInput { events: &evs, typed: &[], mouse: None, width: w, height: h, dt });
        let _ = out;
        p2canvas::natives::with_context(|ctx| ctx.canvas.items.clear());
        frames_on_map += 1;
        phase_frames += 1;
        cur.frames += 1;
        let map = game.level.map.name.clone();
        if cur.maps.last() != Some(&map) {
            cur.maps.push(map.clone());
        }

        // Travel.
        if let Some((m, opts)) = game.take_travel() {
            println!("progress: travel {m}{opts} (frame {frame})");
            let base = m.split('#').next().unwrap_or("").to_ascii_lowercase();
            if base.ends_with("startup") || opts.to_ascii_lowercase().contains("quit") {
                cur.notes.push(format!("travel to main menu ({m}{opts})"));
                game_over = true;
                quit = true;
            } else if game.travel(&m, &opts) {
                game.drain_log(false);
                frames_on_map = 0;
            } else {
                cur.notes.push(format!("travel to {m} failed"));
                quit = true;
            }
        }
        if quit {
            break;
        }

        let (Some(pc), Some(g)) = (game.player_controller, game.game_info) else { continue };
        let vm = &mut game.world.vm;
        if !vm.class_is_a(vm.objects[g].class, "P2GameInfoSingle") {
            continue;
        }
        // Debug path for the errand cheats (the shipped game's EnableDebugMenu).
        if !vm.get_prop(pc, "bEnableDebugMenu").is_some_and(|v| v.as_bool()) {
            vm.set_prop(pc, "bEnableDebugMenu", Value::Bool(true));
        }
        // Skip cutscenes as a player would (not during screens: there a held skip keeps
        // re-arming the screen's delayed state change, so it never ends).
        let screen = current_screen(vm, pc);
        if screen.is_none() {
            vm.set_prop(pc, "bWantsToSkip", Value::Byte(1));
        }
        if let Some(s) = screen {
            if frames_on_map % 300 == 0 {
                println!(
                    "progress: frame {frame} screen {} state {} on {map} delayed {:?} delay {:?} now {:?} running {:?}",
                    vm.object_name(s),
                    vm.state_name(s),
                    vm.get_prop(s, "bDelayedGoto"),
                    vm.get_prop(s, "DelayTime"),
                    vm.level.and_then(|l| vm.get_prop(l, "TimeSecondsAlways")),
                    vm.get_prop(s, "bRequiresTick")
                );
            }
            // Press its end key, but not while a delayed state change is pending (a press calls
            // ChangeExistingDelay, which would keep pushing the fade out back).
            let pending = vm.get_prop(s, "bDelayedGoto").is_some_and(|v| v.as_bool());
            if frames_on_map % 20 == 10 && !pending {
                // The screen's end keys (MapScreen: the QuickUseMap binding) or Enter.
                let key = match vm.get_prop(s, "EndKeys") {
                    Some(Value::Array(k)) if !k.is_empty() => Key(k[0].as_int() as u8),
                    _ => Key::ENTER,
                };
                events.push(InputEvent { key, action: Action::Press, delta: 0.0 });
                events.push(InputEvent { key, action: Action::Release, delta: 0.0 });
            }
            continue;
        }

        // The GameState is created/picked up in PostTravel; until then the day is unknown.
        let Some(day) = current_day(vm, Some(g)) else { continue };
        if day != cur.day {
            // New day started (or the first one).
            if cur.day >= 0 {
                cur.ended = true;
                cur.new_errors = diff(&counts(&vm.script_errors), &errs0);
                cur.new_natives = diff(&counts(&vm.missing_natives), &nat0);
                println!("progress: day {} ({}) ended after {} frames, maps {:?}", cur.day, cur.name, cur.frames, cur.maps);
                let last_map = cur.maps.last().cloned();
                reports.push(std::mem::take(&mut cur));
                if let Some(m) = last_map {
                    cur.maps.push(m);
                }
            }
            errs0 = counts(&vm.script_errors);
            nat0 = counts(&vm.missing_natives);
            cur.day = day;
            cur.name = days(vm, g).get(day.max(0) as usize).and_then(|&d| vm.get_prop(d, "UniqueName")).map(|v| v.as_str()).unwrap_or_default();
            cur.errands = errands(vm, g, day);
            println!("progress: day {day} ({}) on {map}, errands {:?}", cur.name, cur.errands);
            phase = 0;
            phase_frames = 0;
            if last_day.is_some_and(|l| day > l) {
                break;
            }
        }
        let pawn = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()).filter(|&p| !vm.objects[p].deleted);
        if phase_frames % 600 == 0 {
            println!("progress: frame {frame} day {day} phase {phase} on {map} pc {} pawn {:?}", vm.state_name(pc), pawn.map(|p| vm.object_name(p)));
        }
        match phase {
            0 if phase_frames > 60 && pawn.is_some() => {
                call(vm, g, "SetTodaysErrandsComplete", vec![]);
                cur.errands = errands(vm, g, day);
                let ready = vm.call(vm.objects[g].class, Some(g), "IsPlayerReadyForHome", vec![]).map(|v| v.as_bool()).unwrap_or(false);
                cur.notes.push(format!("errands forced complete: {}/{} (ready for home {ready})", cur.errands.iter().filter(|e| e.1).count(), cur.errands.len()));
                println!("progress: day {day}: {}", cur.notes.last().unwrap());
                call(vm, g, "AtPlayerHouse", vec![Value::Object(Some(pc)), Value::Bool(true)]);
                phase = 1;
                phase_frames = 0;
            }
            1 if phase_frames > 30 * 120 => {
                cur.notes.push(format!("no day change 120 s after going home (on {map})"));
                println!("progress: day {day}: stuck on {map}");
                break;
            }
            _ => {}
        }
    }
    if !cur.maps.is_empty() {
        let vm = &game.world.vm;
        cur.new_errors = diff(&counts(&vm.script_errors), &errs0);
        cur.new_natives = diff(&counts(&vm.missing_natives), &nat0);
        cur.ended = game_over;
        reports.push(cur);
    }

    let mut s = String::from("day\tname\tended\tframes\tmaps\terrands\tnew_script_errors\tnew_missing_natives\tnotes\n");
    for r in &reports {
        let top = |m: &BTreeMap<String, u64>| m.iter().map(|(k, n)| format!("{k} x{n}")).collect::<Vec<_>>().join("; ");
        let _ = writeln!(
            s,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.day,
            r.name,
            r.ended,
            r.frames,
            r.maps.join(" > "),
            r.errands.iter().map(|(n, c)| format!("{n}{}", if *c { "+" } else { "-" })).collect::<Vec<_>>().join(" "),
            top(&r.new_errors).replace(['\t', '\n'], " "),
            top(&r.new_natives),
            r.notes.join(" | ").replace(['\t', '\n'], " ")
        );
    }
    std::fs::create_dir_all(out_dir).ok();
    std::fs::write(out_dir.join("progress.tsv"), &s).ok();
    println!("\n{s}");
    let days_ended = reports.iter().filter(|r| r.ended).count();
    let errors: u64 = reports.iter().flat_map(|r| r.new_errors.values()).sum();
    println!("progress: {days_ended} day(s) completed, {errors} new script errors, {:.1}s", t0.elapsed().as_secs_f32());
    if days_ended >= reports.len().saturating_sub(1).max(1) && errors == 0 { 0 } else { 1 }
}
