//! Travel inventory: what UE2 carries from one level to the next with the local player.
//!
//! On a level change the engine keeps the player pawn's `travel` properties and its inventory
//! chain (each item's class and `travel` properties). In the new level, after login spawned the
//! pawn, every carried item is spawned again, gets `TravelPreAccept` (Inventory: `GiveTo(Owner)`),
//! the GameInfo gets `AcceptInventory(Pawn)`, then each item and the pawn get `TravelPostAccept`.
//! POSTAL 2 depends on this: `P2GameInfoSingle.SendPlayerTo` puts the GameState (day, completed
//! errands, haters, ...) into the pawn's inventory before travelling and `PostTravel` takes it
//! back out; errand items (paycheck, milk) and money ride along the same way.
//!
//! Items whose class the new pawn already owns (default inventory such as hands, money, map)
//! are not duplicated: the carried `travel` values are copied onto the existing item instead.
//! Object references between carried objects are remapped; references to anything else (the
//! old level's actors) become None. `ClientTravel(URL, Type, bItems=false)` skips the carry.

use crate::host::GameHost;
use std::cell::{Cell, RefCell};
use uscript::objects::prop_flags;
use uscript::vm::{ClassId, ObjId, Value, Vm};

/// Marker for a reference to carried object `i` (0 is the pawn) inside a captured value.
const REF_TAG: &str = "\u{1}travel-ref";

#[derive(Debug, Clone)]
struct Carried {
    class: ClassId,
    props: Vec<(usize, Value)>,
}

#[derive(Debug, Default, Clone)]
pub struct Carry {
    pawn: Option<Carried>,
    items: Vec<Carried>,
}

impl Carry {
    pub fn is_empty(&self) -> bool {
        self.pawn.is_none() && self.items.is_empty()
    }
    /// Class names of the carried inventory items.
    pub fn item_classes(&self, vm: &Vm) -> Vec<String> {
        self.items.iter().map(|c| vm.classes[c.class].name.clone()).collect()
    }
}

#[derive(Debug, Default, Clone)]
pub struct TravelStats {
    pub carried: usize,
    pub spawned: usize,
    pub merged: usize,
}

thread_local! {
    static ITEMS: Cell<bool> = const { Cell::new(true) };
    pub static STATS: RefCell<TravelStats> = RefCell::new(TravelStats::default());
}

/// `ClientTravel`'s `bItems`: whether the next travel carries inventory.
pub fn set_carry_items(items: bool) {
    ITEMS.with(|c| c.set(items));
}

/// `PlayerController.ClientTravel(URL, TravelType, bItems)` as the engine runs it: records whether
/// inventory travels and returns the `open` command that queues the level change. The front end
/// answers it the same way; this is the fallback when no front end is installed (probes, smoke).
pub fn client_travel(args: &[uscript::vm::Value]) -> String {
    set_carry_items(args.get(2).is_none_or(|v| v.as_bool()));
    format!("open {}", args.first().map(|v| v.as_str()).unwrap_or_default())
}

fn live(vm: &Vm, o: ObjId) -> bool {
    vm.objects.get(o).is_some_and(|x| !x.deleted)
}

/// The pawn's inventory chain (`Inventory` links), stopping on loops.
fn chain(vm: &Vm, pawn: ObjId) -> Vec<ObjId> {
    let mut out = Vec::new();
    let mut cur = vm.get_prop(pawn, "Inventory").and_then(|v| v.as_obj());
    while let Some(i) = cur {
        if !live(vm, i) || out.contains(&i) || out.len() > 256 {
            break;
        }
        out.push(i);
        cur = vm.get_prop(i, "Inventory").and_then(|v| v.as_obj());
    }
    out
}

fn encode(v: &Value, ids: &[ObjId]) -> Value {
    match v {
        Value::Object(Some(o)) => match ids.iter().position(|x| x == o) {
            Some(i) => Value::Struct(vec![(REF_TAG.into(), Value::Int(i as i32))]),
            None => Value::Object(None),
        },
        Value::Array(a) => Value::Array(a.iter().map(|x| encode(x, ids)).collect()),
        Value::Struct(f) => Value::Struct(f.iter().map(|(k, x)| (k.clone(), encode(x, ids))).collect()),
        Value::DelegateTo { .. } => Value::Delegate,
        other => other.clone(),
    }
}

fn decode(v: &Value, ids: &[Option<ObjId>]) -> Value {
    match v {
        Value::Struct(f) if f.len() == 1 && f[0].0 == REF_TAG => Value::Object(ids.get(f[0].1.as_int() as usize).copied().flatten()),
        Value::Array(a) => Value::Array(a.iter().map(|x| decode(x, ids)).collect()),
        Value::Struct(f) => Value::Struct(f.iter().map(|(k, x)| (k.clone(), decode(x, ids))).collect()),
        other => other.clone(),
    }
}

fn capture_one(vm: &Vm, o: ObjId, ids: &[ObjId]) -> Carried {
    let class = vm.objects[o].class;
    let ci = &vm.classes[class];
    let props = ci
        .slots
        .iter()
        .enumerate()
        .filter(|(_, s)| s.flags & prop_flags::TRAVEL != 0)
        .map(|(i, _)| (i, encode(&vm.objects[o].slots[i], ids)))
        .collect();
    Carried { class, props }
}

/// Records the local player's pawn and inventory before the level is torn down.
pub fn capture(vm: &Vm, pc: Option<ObjId>) -> Carry {
    let carry_items = ITEMS.with(|c| c.replace(true));
    let Some(pawn) = pc.filter(|&c| live(vm, c)).and_then(|c| vm.get_prop(c, "Pawn")).and_then(|v| v.as_obj()).filter(|&p| live(vm, p)) else {
        return Carry::default();
    };
    if vm.get_prop(pawn, "Health").is_some_and(|h| h.as_int() <= 0) {
        return Carry::default();
    }
    let items = if carry_items { chain(vm, pawn) } else { Vec::new() };
    let mut ids = vec![pawn];
    ids.extend(&items);
    let carry = Carry { pawn: Some(capture_one(vm, pawn, &ids)), items: items.iter().map(|&i| capture_one(vm, i, &ids)).collect() };
    STATS.with(|s| s.borrow_mut().carried = carry.items.len());
    carry
}

fn apply(vm: &mut Vm, o: ObjId, c: &Carried, ids: &[Option<ObjId>]) {
    if vm.objects[o].class != c.class {
        return;
    }
    for (slot, v) in &c.props {
        let v = decode(v, ids);
        if let Some(s) = vm.objects[o].slots.get_mut(*slot) {
            *s = v;
        }
    }
}

/// Gives the carried inventory to the local player's new pawn (after login) and runs the
/// travel acceptance events.
pub fn restore(vm: &mut Vm, host: &GameHost, pc: Option<ObjId>, carry: Carry) {
    let _ = host;
    if carry.is_empty() {
        return;
    }
    let Some(pawn) = pc.filter(|&c| live(vm, c)).and_then(|c| vm.get_prop(c, "Pawn")).and_then(|v| v.as_obj()).filter(|&p| live(vm, p)) else {
        return;
    };
    let existing = chain(vm, pawn);
    let loc = vm.actor_loc(pawn);
    // Target object per carried index (0 = pawn): an existing item of the same class, or a new one.
    let mut ids: Vec<Option<ObjId>> = vec![Some(pawn)];
    let mut fresh = Vec::new();
    let mut used = Vec::new();
    let (mut spawned, mut merged) = (0, 0);
    for c in &carry.items {
        let have = existing.iter().copied().find(|&e| vm.objects[e].class == c.class && !used.contains(&e));
        let target = match have {
            Some(e) => {
                used.push(e);
                merged += 1;
                Some(e)
            }
            None => {
                let p = uscript::host::SpawnParams { owner: Some(pawn), instigator: Some(pawn), location: loc, ..Default::default() };
                let o = vm.spawn_actor(c.class, p, true);
                if let Some(o) = o {
                    fresh.push(o);
                    spawned += 1;
                }
                o
            }
        };
        ids.push(target);
    }
    if let Some(p) = &carry.pawn {
        // Keep the pawn's own links (Weapon / SelectedItem) only when they resolve.
        apply(vm, pawn, p, &ids);
    }
    for (k, c) in carry.items.iter().enumerate() {
        if let Some(o) = ids[k + 1] {
            apply(vm, o, c, &ids);
        }
    }
    for &o in &fresh {
        if live(vm, o) {
            vm.set_prop(o, "Owner", Value::Object(Some(pawn)));
            vm.fire_event(o, "TravelPreAccept", vec![]);
        }
    }
    if let Some(g) = vm.level.and_then(|l| vm.get_prop(l, "Game")).and_then(|v| v.as_obj()) {
        vm.fire_event(g, "AcceptInventory", vec![Value::Object(Some(pawn))]);
    }
    for &o in &fresh {
        if live(vm, o) {
            vm.fire_event(o, "TravelPostAccept", vec![]);
        }
    }
    vm.fire_event(pawn, "TravelPostAccept", vec![]);
    STATS.with(|s| {
        let mut s = s.borrow_mut();
        s.spawned = spawned;
        s.merged = merged;
    });
    if std::env::var_os("P2GAME_TRAVEL_LOG").is_some() {
        println!("travel: carried {} items ({spawned} spawned, {merged} merged): {:?}", carry.items.len(), carry.item_classes(vm));
    }
}
