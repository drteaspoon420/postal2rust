//! Editor frozen-move check: boots a map, moves NPCs 400uu without ticking the world (as the
//! editor does while time is frozen), then re-places bone attachments the way the dev editor
//! does and reports how far every actor based on each NPC (head, hat, held weapon) is from it.
//! `frozen_move_probe [Map] [N]`
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::sync::Arc;
use uscript::vm::{ObjId, Vm};

fn based(vm: &Vm, p: ObjId) -> Vec<ObjId> {
    // Direct and nested (hat on head) attachments.
    let mut out = Vec::new();
    let mut frontier = vec![p];
    while let Some(b) = frontier.pop() {
        for &a in &vm.actors {
            if !vm.objects[a].deleted && vm.get_prop(a, "Base").and_then(|v| v.as_obj()) == Some(b) && !out.contains(&a) && a != p {
                out.push(a);
                frontier.push(a);
            }
        }
    }
    out
}

fn report(vm: &Vm, tag: &str, p: ObjId, kids: &[ObjId]) -> f32 {
    let pl = glam::Vec3::from(vm.actor_loc(p));
    let mut worst = 0.0f32;
    for &k in kids {
        let d = (glam::Vec3::from(vm.actor_loc(k)) - pl).length();
        worst = worst.max(d);
        println!("  {tag} {} {} ({}) bone {:?} d {d:.0}", vm.object_name(p), vm.object_name(k), vm.classes[vm.objects[k].class].name, vm.get_prop(k, "AttachmentBone").map(|v| v.as_name()));
    }
    worst
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let map = a.first().cloned().unwrap_or_else(|| "suburbs-3".into());
    let n: usize = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(4);
    let root = p2game::game_root(None);
    p2game::host::register_level_hook(p2game::physics::level_hook);
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(&map).cloned().expect("map not found");
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let mut game = Game::new(world, lv, Some("GameTypes.GameSinglePlayer"));
    game.begin_play("");
    for _ in 0..30 {
        game.tick(1.0 / 30.0);
        game.drain_log(false);
    }
    let host = game.host.clone();
    let vm = &mut game.world.vm;
    let player = game.player_controller.and_then(|pc| vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()));
    let npcs: Vec<ObjId> = vm.actors.iter().copied().filter(|&x| !vm.objects[x].deleted && Some(x) != player && vm.class_is_a(vm.objects[x].class, "P2MoCapPawn") && host.anims.borrow().contains_key(&x) && !based(vm, x).is_empty()).take(n).collect();
    let (mut before_fix, mut after_fix) = (0.0f32, 0.0f32);
    for &p in &npcs {
        let kids = based(vm, p);
        report(vm, "start", p, &kids);
        let l = vm.actor_loc(p);
        let to = [l[0] + 400.0, l[1], l[2]];
        if !uscript::host::Host::move_actor(&*host, vm, p, to, false) {
            vm.set_prop(p, "Location", uscript::vm::Value::Vector(to));
        }
        before_fix = before_fix.max(report(vm, "moved", p, &kids));
    }
    host.update_attachments(vm);
    for &p in &npcs {
        let kids = based(vm, p);
        after_fix = after_fix.max(report(vm, "replaced", p, &kids));
    }
    println!("FROZEN_MOVE npcs {} worst attachment distance: after move {before_fix:.0}uu, after re-place {after_fix:.0}uu", npcs.len());
}
