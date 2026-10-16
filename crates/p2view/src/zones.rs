//! UE2 zone visibility for static mesh actors. The renderer only draws zones reachable from the
//! camera's zone through portal surfaces (zone 0, outside any zone, always counts as visible).
//! Without it, set pieces in sealed-off zones far from the camera were drawn over the sky and
//! cut off by the fog far plane as the camera moved (intro.fuk: the drive into Paradise sits in
//! zone 27, which has no portals; its 218 statics are the only ones the original draws there,
//! the other 450 in zones 2/23/24... appeared and vanished at 952 m as "map culling in").
//!
//! Zone numbers are bytes on the BSP nodes and some shipped maps use more than 64 of them
//! (suburbs-3 has 71: the laundromat is zones 68-70), so the sets are 256 bits wide. With a
//! 64-bit set the laundromat's zones were clamped to 63, which zone 1 cannot reach, and its
//! washers, dryers and floor vanished from outside, leaving only the boundary walls.

use bevy::prelude::*;
use std::sync::Arc;

/// BSP zone number of a static actor.
#[derive(Component, Clone, Copy)]
pub struct ZoneTag(pub u8);

/// Marks an entity whose actor script has hidden (bHidden): zone culling must not show it again.
#[derive(Component, Clone, Copy)]
pub struct ActorHidden;

/// A set of zone numbers (0..=255).
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct ZoneSet([u64; 4]);

impl ZoneSet {
    pub const ALL: ZoneSet = ZoneSet([u64::MAX; 4]);

    pub fn contains(&self, z: u8) -> bool {
        self.0[(z >> 6) as usize] & (1u64 << (z & 63)) != 0
    }

    pub fn insert(&mut self, z: u8) {
        self.0[(z >> 6) as usize] |= 1u64 << (z & 63);
    }

    fn union(&mut self, o: &ZoneSet) {
        for (a, b) in self.0.iter_mut().zip(o.0) {
            *a |= b;
        }
    }

    fn iter(&self) -> impl Iterator<Item = u8> + '_ {
        (0..=255u8).filter(|&z| self.contains(z))
    }
}

impl std::fmt::LowerHex for ZoneSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}{:016x}{:016x}{:016x}", self.0[3], self.0[2], self.0[1], self.0[0])
    }
}

#[derive(Resource)]
pub struct ZoneGraph {
    adj: Vec<ZoneSet>,
    bsp: Option<Arc<p2collide::bsp::Bsp>>,
    last: Option<usize>,
}

impl ZoneGraph {
    pub fn new(portals: &[(u8, u8)], bsp: Option<Arc<p2collide::bsp::Bsp>>) -> Self {
        let mut adj = vec![ZoneSet::default(); 256];
        for &(a, b) in portals {
            adj[a as usize].insert(b);
            adj[b as usize].insert(a);
        }
        ZoneGraph { adj, bsp, last: None }
    }

    /// Zones reachable from `z` through portals, not passing through zone 0 (the unzoned void).
    pub fn reachable(&self, z: usize) -> ZoneSet {
        if z == 0 || z > 255 {
            return ZoneSet::ALL;
        }
        let mut seen = ZoneSet::default();
        seen.insert(z as u8);
        let mut todo = vec![z as u8];
        while let Some(c) = todo.pop() {
            let n = self.adj[c as usize];
            for o in n.iter() {
                if o != 0 && !seen.contains(o) {
                    todo.push(o);
                }
            }
            seen.union(&n);
        }
        seen.insert(0);
        seen
    }
}

/// Hides static actors whose zone the camera's zone cannot see. `P2VIEW_NO_ZONE_CULL` disables it.
pub fn zone_cull(mut graph: Option<ResMut<ZoneGraph>>, cams: Query<(&GlobalTransform, &Camera), With<Camera3d>>, mut q: Query<(&ZoneTag, &mut Visibility, Has<ActorHidden>)>, mut unhidden: RemovedComponents<ActorHidden>) {
    let Some(g) = graph.as_mut() else { return };
    if std::env::var_os("P2VIEW_NO_ZONE_CULL").is_some() {
        return;
    }
    // An actor script showed again needs its zone visibility re-applied.
    if unhidden.read().count() > 0 {
        g.last = None;
    }
    let Some(bsp) = g.bsp.clone() else { return };
    // The main world camera: the active 3D camera with the highest order (the sky camera is -1).
    let Some((tf, _)) = cams.iter().filter(|(_, c)| c.is_active).max_by_key(|(_, c)| c.order) else { return };
    let p = tf.translation() * crate::scene::UU_PER_M;
    let zone = bsp.point_zone(Vec3::new(p.x, p.z, p.y));
    if g.last == Some(zone) {
        return;
    }
    g.last = Some(zone);
    let vis = g.reachable(zone);
    if std::env::var_os("P2VIEW_ZONE_TRACE").is_some() {
        println!("ZONECULL camera zone {zone} visible {vis:x}");
    }
    for (t, mut v, hidden) in &mut q {
        let want = if !hidden && vis.contains(t.0) { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want {
            *v = want;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones_above_63_are_reached() {
        // suburbs-3 laundromat: 1-68, 68-69, 69-70, 70-1; zone 63 hangs off 64, which joins 1.
        let g = ZoneGraph::new(&[(1, 68), (68, 69), (69, 70), (1, 70), (1, 64), (63, 64), (62, 63)], None);
        let v = g.reachable(1);
        for z in [0, 1, 62, 63, 64, 68, 69, 70] {
            assert!(v.contains(z), "zone {z}");
        }
        assert!(!v.contains(65));
        let v = g.reachable(69);
        assert!(v.contains(1) && v.contains(62) && !v.contains(65));
    }
}
