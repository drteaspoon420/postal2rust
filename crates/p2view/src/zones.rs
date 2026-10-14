//! UE2 zone visibility for static mesh actors. The renderer only draws zones reachable from the
//! camera's zone through portal surfaces (zone 0, outside any zone, always counts as visible).
//! Without it, set pieces in sealed-off zones far from the camera were drawn over the sky and
//! cut off by the fog far plane as the camera moved (intro.fuk: the drive into Paradise sits in
//! zone 27, which has no portals; its 218 statics are the only ones the original draws there,
//! the other 450 in zones 2/23/24... appeared and vanished at 952 m as "map culling in").

use bevy::prelude::*;
use std::sync::Arc;

/// BSP zone number of a static actor.
#[derive(Component, Clone, Copy)]
pub struct ZoneTag(pub u8);

#[derive(Resource)]
pub struct ZoneGraph {
    adj: Vec<u64>,
    bsp: Option<Arc<p2collide::bsp::Bsp>>,
    last: Option<usize>,
}

impl ZoneGraph {
    pub fn new(portals: &[(u8, u8)], bsp: Option<Arc<p2collide::bsp::Bsp>>) -> Self {
        let mut adj = vec![0u64; 64];
        for &(a, b) in portals {
            if a < 64 && b < 64 {
                adj[a as usize] |= 1 << b;
                adj[b as usize] |= 1 << a;
            }
        }
        ZoneGraph { adj, bsp, last: None }
    }

    /// Zones reachable from `z` through portals, not passing through zone 0 (the unzoned void).
    pub fn reachable(&self, z: usize) -> u64 {
        if z == 0 || z >= 64 {
            return u64::MAX;
        }
        let mut seen = 1u64 << z;
        let mut todo = vec![z];
        while let Some(c) = todo.pop() {
            let mut n = self.adj[c] & !seen & !1;
            seen |= self.adj[c];
            while n != 0 {
                let o = n.trailing_zeros() as usize;
                n &= n - 1;
                todo.push(o);
            }
        }
        seen | 1
    }
}

/// Hides static actors whose zone the camera's zone cannot see. `P2VIEW_NO_ZONE_CULL` disables it.
pub fn zone_cull(mut graph: Option<ResMut<ZoneGraph>>, cams: Query<(&GlobalTransform, &Camera), With<Camera3d>>, mut q: Query<(&ZoneTag, &mut Visibility)>) {
    let Some(g) = graph.as_mut() else { return };
    if std::env::var_os("P2VIEW_NO_ZONE_CULL").is_some() {
        return;
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
        println!("ZONECULL camera zone {zone} visible {vis:#x}");
    }
    for (t, mut v) in &mut q {
        let want = if vis & (1u64 << t.0.min(63)) != 0 { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want {
            *v = want;
        }
    }
}
