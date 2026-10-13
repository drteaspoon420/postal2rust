//! Distance fog of the zone the camera is in, as UE2 renders it: every frame the viewer's zone
//! (BSP point query) picks the `DistanceFogColor/Start/End` the scene is fogged with, read live
//! from the zone actor in the VM. Script rewrites those: `P2GameInfoSingle` startup calls
//! `SetZoneFogPlanes`, which gives every zone with `bUseGlobalFog` the ini's
//! `GeneralFogStart/End` (the shipped Postal2.ini has 50000/50000, i.e. no visible fog; the
//! map's own 0..9000 made Compound's hills fog-grey), and the sniper zoom swaps in
//! `SniperFogStart/End`.
//!
//! p2view's scene builder only knew one level-wide zone (the LevelInfo when it sets fog), which
//! also washed zones with their own darker fog in grey: ngt-Church's start zone fogs to 16,17,18
//! while the LevelInfo says 66,68,75. Zone 0 (no zone actor) uses the LevelInfo.

use crate::window::{FlyCam, Sim};
use bevy::prelude::*;
use uscript::vm::{ObjId, Value};

type Fog = ([u8; 3], f32, f32);

/// Fog distance (m) used for "no fog": beyond any map.
const NO_FOG: f32 = 1.0e4;

#[derive(Default)]
pub(crate) struct ZoneFog {
    last: Option<Fog>,
}

/// (sRGB colour, start m, end m) of a live ZoneInfo / LevelInfo object, if it has distance fog.
fn fog_of(vm: &uscript::vm::Vm, z: ObjId) -> Option<Fog> {
    if !vm.get_prop(z, "bDistanceFog").is_some_and(|v| v.as_bool()) {
        return None;
    }
    let start = vm.get_prop(z, "DistanceFogStart").map(|v| v.as_float()).unwrap_or(0.0);
    let end = vm.get_prop(z, "DistanceFogEnd").map(|v| v.as_float()).unwrap_or(0.0);
    // GeneralFogStart == GeneralFogEnd (50000 in the shipped ini) is valid: clear up to there.
    if end < start || end <= 0.0 {
        return None;
    }
    let c = match vm.get_prop(z, "DistanceFogColor") {
        Some(Value::Struct(s)) => {
            let ch = |n: &str| s.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.as_int().clamp(0, 255) as u8).unwrap_or(0);
            [ch("R"), ch("G"), ch("B")]
        }
        _ => return None,
    };
    let u = p2view::scene::UU_PER_M;
    Some((c, start / u, end / u))
}

pub(crate) fn zone_fog(sim: NonSend<Sim>, mut state: Local<ZoneFog>, mut cams: Query<(&Transform, &mut DistanceFog), With<FlyCam>>) {
    if std::env::var_os("P2VIEW_LEVEL_FOG").is_some() {
        return;
    }
    let Some(col) = p2game::physics::level_collision() else { return };
    let Ok((tf, mut fog)) = cams.single_mut() else { return };
    let p = tf.translation * p2view::scene::UU_PER_M;
    let zone = col.bsp.point_zone(Vec3::new(p.x, p.z, p.y));
    let vm = &sim.game.world.vm;
    let level = &sim.game.level;
    let zone_obj = col.zones.get(zone).copied().flatten().and_then(|e| level.objects.get(&(e as usize)).copied());
    let want = zone_obj.or(level.level_info).and_then(|z| fog_of(vm, z));
    // No fog in this zone: UE2 draws it unfogged (the far clip stays at the fog end, so keep the
    // distance and only push the fog out).
    let want = want.unwrap_or_else(|| {
        let (c, _, end) = state.last.unwrap_or(([0, 0, 0], 0.0, NO_FOG));
        (c, end.max(NO_FOG), end.max(NO_FOG) + 1.0)
    });
    if state.last.is_some_and(|l| l == want || (want.1 >= NO_FOG && l.1 >= NO_FOG)) {
        return;
    }
    state.last = Some(want);
    if std::env::var_os("P2GAME_ZONE_TRACE").is_some() {
        let z = zone_obj.or(level.level_info);
        println!(
            "ZONEFOG zone {zone} obj {zone_obj:?} fog {want:?} raw {:?} {:?} {:?} {:?}",
            z.and_then(|z| vm.get_prop(z, "bDistanceFog")),
            z.and_then(|z| vm.get_prop(z, "DistanceFogStart")),
            z.and_then(|z| vm.get_prop(z, "DistanceFogEnd")),
            z.and_then(|z| vm.get_prop(z, "DistanceFogColor"))
        );
    }
    let (c, start, end) = want;
    // Experiment knob: P2VIEW_FOG_SCALE multiplies the fog distances (100 = effectively off).
    let k = std::env::var("P2VIEW_FOG_SCALE").ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.0);
    let (start, end) = (start * k, end * k);
    fog.color = Color::srgb_u8(c[0], c[1], c[2]);
    fog.falloff = FogFalloff::Linear { start, end: end.max(start + 1.0) };
}
