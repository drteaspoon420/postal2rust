//! First-person element wheel. Tapping the middle button self-casts (or uses
//! the staff special); holding it opens a radial wheel and locks the view.
//! Moving the mouse past an element queues it and re-centres the wheel at the
//! pointer, so quick flicks string elements together.
use bevy::prelude::*;
use std::collections::VecDeque;

/// Key slots (Water, Life, Shield, Cold, Lightning, Arcane, Earth, Fire) of
/// the wheel sectors, clockwise from the top. Opposing elements face each
/// other: Water/Lightning, Life/Arcane, Cold/Fire, Shield/Earth.
pub(super) const SECTORS: [usize; 8] = [0, 1, 3, 2, 4, 5, 7, 6];
/// Hold time before the wheel opens; a shorter press is a self-cast tap.
const OPEN_AFTER: f32 = 0.18;
/// Pointer travel from the centre that picks a sector (logical pixels).
pub(super) const PICK: f32 = 60.;
/// Icon ring radius as drawn.
pub(super) const RADIUS: f32 = 70.;

#[derive(Default)]
pub(super) struct Wheel {
    pub open: bool,
    held: f32,
    picked: bool,
    pub center: Vec2,
    pub cursor: Vec2,
    pending: VecDeque<usize>,
    pulse_on: bool,
    self_cast_frames: u8,
    /// Last picked sector and the time since, for a brief highlight.
    pub flash: Option<(usize, f32)>,
}

impl Wheel {
    /// Advance with this frame's middle-button state and mouse motion; returns
    /// the element keys to press this frame and whether to self-cast.
    pub fn update(&mut self, middle: bool, motion: Vec2, screen: Vec2, dt: f32) -> ([bool; 8], bool) {
        if let Some((_, t)) = &mut self.flash {
            *t += dt;
            if *t > 0.25 {
                self.flash = None;
            }
        }
        if middle {
            self.held += dt;
            if !self.open && self.held >= OPEN_AFTER {
                self.open = true;
                self.center = screen / 2.;
                self.cursor = self.center;
            }
            if self.open {
                self.cursor = (self.cursor + motion).clamp(Vec2::ZERO, screen);
                let d = self.cursor - self.center;
                if d.length() > PICK {
                    // 0 at the top, clockwise (screen y grows downwards).
                    let angle = d.x.atan2(-d.y).rem_euclid(std::f32::consts::TAU);
                    let sector = (angle / (std::f32::consts::TAU / 8.)).round() as usize % 8;
                    self.pending.push_back(SECTORS[sector]);
                    self.picked = true;
                    self.flash = Some((sector, 0.));
                    // Re-centre at the pointer, kept on screen so repeated
                    // flicks in one direction keep working.
                    let margin = Vec2::splat(RADIUS + 30.);
                    self.center = self.cursor.clamp(margin, (screen - margin).max(margin));
                    self.cursor = self.center;
                }
            }
        } else {
            if self.held > 0. && !self.open && !self.picked {
                self.self_cast_frames = 2;
            }
            self.held = 0.;
            self.open = false;
            self.picked = false;
        }
        // One frame down, one up: repeated elements need a fresh press each.
        let mut keys = [false; 8];
        if self.pulse_on {
            self.pulse_on = false;
        } else if let Some(k) = self.pending.pop_front() {
            keys[k] = true;
            self.pulse_on = true;
        }
        let self_cast = self.self_cast_frames > 0;
        self.self_cast_frames = self.self_cast_frames.saturating_sub(1);
        (keys, self_cast)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tap_self_casts_and_flicks_queue_elements() {
        let screen = Vec2::new(800., 600.);
        let mut w = Wheel::default();
        // A short tap self-casts.
        w.update(true, Vec2::ZERO, screen, 0.05);
        let (_, cast) = w.update(false, Vec2::ZERO, screen, 0.05);
        assert!(cast);
        // Hold, then flick up (Water) twice and right (Cold).
        for _ in 0..5 {
            w.update(true, Vec2::ZERO, screen, 0.05);
        }
        assert!(w.open);
        let mut pressed = Vec::new();
        for motion in [Vec2::new(0., -70.), Vec2::new(0., -70.), Vec2::new(70., 0.), Vec2::ZERO, Vec2::ZERO, Vec2::ZERO, Vec2::ZERO] {
            let (keys, cast) = w.update(true, motion, screen, 0.016);
            assert!(!cast);
            pressed.extend(keys.iter().enumerate().filter(|(_, k)| **k).map(|(i, _)| i));
        }
        assert_eq!(pressed, vec![0, 0, 3]);
        let (_, cast) = w.update(false, Vec2::ZERO, screen, 0.016);
        assert!(!cast, "releasing after picking must not self-cast");
    }
}
