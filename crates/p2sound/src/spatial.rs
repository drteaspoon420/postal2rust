//! Spatial math matching UE2's audio behaviour (positions in Unreal units, Z up).
//!
//! UE2 drives OpenAL with an inverse-distance-clamped model: reference distance = the sound's
//! radius (full volume inside it), rolloff from the audio config (`Rolloff=0.9` in POSTAL 2's
//! ini), maximum distance `Radius * GAudioMaxRadiusMultiplier`, beyond which the sound is not
//! audible (culled). POSTAL 2's Core.dll initialises `GAudioMaxRadiusMultiplier = 100` and
//! `GAudioDefaultRadius = 80` (read from the shipped binary's .data), so a radius-50 katydid
//! carries ~5000 uu, quietly: gain = r / (r + rolloff * (d - r)).

use glam::Vec3;

/// Tunables of the distance model.
#[derive(Debug, Clone, Copy)]
pub struct AttenuationParams {
    /// OpenAL rolloff factor (`[ALAudio.ALAudioSubsystem] Rolloff`).
    pub rolloff: f32,
    /// Sounds fade to zero at `radius * max_radius_multiplier` and are culled beyond it.
    pub max_radius_multiplier: f32,
    /// Radius used when a script passes 0.
    pub default_radius: f32,
    /// Radius multiplier applied when the occlusion trace is blocked.
    pub occluded_radius_scale: f32,
}

impl Default for AttenuationParams {
    fn default() -> Self {
        Self { rolloff: 0.9, max_radius_multiplier: 100.0, default_radius: 80.0, occluded_radius_scale: 0.35 }
    }
}

/// Gain (0..1) of a sound of `radius` heard at `distance`.
pub fn distance_gain(distance: f32, radius: f32, p: &AttenuationParams) -> f32 {
    let r = radius.max(1.0);
    if distance <= r {
        return 1.0;
    }
    let max = r * p.max_radius_multiplier.max(1.0);
    if distance >= max {
        return 0.0;
    }
    // OpenAL AL_INVERSE_DISTANCE_CLAMPED with reference distance r.
    (r / (r + p.rolloff * (distance - r))).clamp(0.0, 1.0)
}

/// Listener frame in Unreal space.
#[derive(Debug, Clone, Copy)]
pub struct Listener {
    pub position: Vec3,
    /// Unit forward vector (Unreal +X when unrotated).
    pub forward: Vec3,
    /// Unit up vector (+Z when unrotated).
    pub up: Vec3,
}

impl Default for Listener {
    fn default() -> Self {
        Self { position: Vec3::ZERO, forward: Vec3::X, up: Vec3::Z }
    }
}

impl Listener {
    /// Unreal is left-handed: with X forward and Z up, +Y is to the right, which is
    /// `up x forward` (Z x X = Y).
    pub fn right(&self) -> Vec3 {
        self.up.cross(self.forward).normalize_or_zero()
    }
}

/// Stereo pan in -1 (left) .. 1 (right) for a source at `pos`. Sounds closer than `spread`
/// units are pulled toward the centre so nearby sounds do not jump between ears.
pub fn pan(listener: &Listener, pos: Vec3, spread: f32) -> f32 {
    let d = pos - listener.position;
    let len = d.length();
    if len < 1e-3 {
        return 0.0;
    }
    let side = d.dot(listener.right()) / len;
    let near = (len / spread.max(1e-3)).min(1.0);
    (side * near).clamp(-1.0, 1.0)
}

/// Equal-power gains (left, right) for a pan value.
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let a = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    (a.cos(), a.sin())
}

/// Voice priority: the gain the voice would be heard at (louder and closer is more important);
/// `bMaxPriority` wins over all.
pub fn priority(listener: &Listener, pos: Option<Vec3>, volume: f32, radius: f32, max_priority: bool, p: &AttenuationParams) -> f32 {
    let mut f = 1.0;
    if let Some(pos) = pos {
        f = distance_gain(pos.distance(listener.position), radius, p).max(1e-4);
    }
    f * volume + if max_priority { 9999.0 } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attenuation_curve() {
        let p = AttenuationParams::default();
        assert_eq!(distance_gain(0.0, 100.0, &p), 1.0);
        assert_eq!(distance_gain(100.0, 100.0, &p), 1.0);
        // Inverse distance: 100 / (100 + 0.9 * 900) at 10x the radius.
        let g = distance_gain(1000.0, 100.0, &p);
        assert!((g - 100.0 / 910.0).abs() < 1e-4, "{g}");
        // Culled at 100x the radius (GAudioMaxRadiusMultiplier).
        assert!(distance_gain(9999.0, 100.0, &p) > 0.0);
        assert_eq!(distance_gain(10000.0, 100.0, &p), 0.0);
        // Monotonic decreasing.
        let mut last = 1.0;
        for d in (100..=10000).step_by(50) {
            let g = distance_gain(d as f32, 100.0, &p);
            assert!(g <= last + 1e-6);
            last = g;
        }
    }

    #[test]
    fn pan_left_right() {
        let l = Listener::default();
        // Unreal +Y is right.
        assert!(pan(&l, Vec3::new(0.0, 500.0, 0.0), 100.0) > 0.99);
        assert!(pan(&l, Vec3::new(0.0, -500.0, 0.0), 100.0) < -0.99);
        assert!(pan(&l, Vec3::new(500.0, 0.0, 0.0), 100.0).abs() < 1e-5);
        assert!(pan(&l, Vec3::new(-500.0, 0.0, 0.0), 100.0).abs() < 1e-5);
        // Turned to face +Y: a sound at +Y is ahead, one at -X is on the right.
        let turned = Listener { forward: Vec3::Y, ..l };
        assert!(pan(&turned, Vec3::new(0.0, 500.0, 0.0), 100.0).abs() < 1e-5);
        assert!(pan(&turned, Vec3::new(-500.0, 0.0, 0.0), 100.0) > 0.99);
        // Near sounds are centred.
        assert!((pan(&l, Vec3::new(0.0, 10.0, 0.0), 100.0) - 0.1).abs() < 1e-5);
        let (a, b) = pan_gains(0.0);
        assert!((a - b).abs() < 1e-6 && (a * a + b * b - 1.0).abs() < 1e-5);
        assert_eq!(pan_gains(1.0).0.abs() < 1e-6, true);
    }

    #[test]
    fn priority_order() {
        let p = AttenuationParams::default();
        let l = Listener::default();
        let near = priority(&l, Some(Vec3::new(10.0, 0.0, 0.0)), 1.0, 100.0, false, &p);
        let far = priority(&l, Some(Vec3::new(1400.0, 0.0, 0.0)), 1.0, 100.0, false, &p);
        assert!(near > far);
        assert!(priority(&l, Some(Vec3::new(1e6, 0.0, 0.0)), 1.0, 100.0, true, &p) > near);
        assert!((priority(&l, None, 0.5, 100.0, false, &p) - 0.5).abs() < 1e-6);
    }
}
