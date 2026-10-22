//! Portable player state for Postal saves and travel. Assets, collision, VM
//! object ids and render handles are reconstructed in the destination world.
use crate::{Loadout, WizardWorld, magicka_runtime as runtime, magicka_sim as sim};
use serde::{Deserialize, Serialize};
fn white() -> [f32; 3] {
    [1.; 3]
}
use sim::{
    character_health::{CharacterHealth, SelfShield, SelfShieldType},
    magicks::{AbilityEffect, Haste},
    spells::Spell,
    status::{Status, StatusEffect},
};

/// Serde form of `Spell` (the simulation crate carries no serde).
mod spell_repr {
    use super::{Deserialize, Serialize, Spell, sim};
    use serde::{Deserializer, Serializer};
    #[derive(Serialize, Deserialize)]
    struct Repr {
        element: u32,
        mag: [f32; 11],
        magick: u8,
    }
    fn from(s: &Spell) -> Repr {
        Repr { element: s.element.0, mag: s.mag, magick: s.magick as u8 }
    }
    fn into<E: serde::de::Error>(r: Repr) -> Result<Spell, E> {
        Ok(Spell {
            element: sim::spells::Elements(r.element),
            mag: r.mag,
            magick: sim::spells::MagickType::try_from(r.magick)
                .map_err(|_| E::custom("invalid magick type"))?,
        })
    }
    pub fn serialize<S: Serializer>(s: &Spell, ser: S) -> Result<S::Ok, S::Error> {
        from(s).serialize(ser)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Spell, D::Error> {
        into(Repr::deserialize(de)?)
    }
    pub mod list {
        use super::*;
        pub fn serialize<S: Serializer>(s: &[Spell], ser: S) -> Result<S::Ok, S::Error> {
            s.iter().map(from).collect::<Vec<_>>().serialize(ser)
        }
        pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<Spell>, D::Error> {
            Vec::<Repr>::deserialize(de)?.into_iter().map(into).collect()
        }
    }
}

macro_rules! float_state {
    ($name:ident, $source:ty, $($field:ident),+ $(,)?) => {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        struct $name { $($field: f32),+ }
        impl $name {
            fn capture(value: &$source) -> Self { Self { $($field: value.$field),+ } }
            fn restore(&self, value: &mut $source) { $(value.$field = self.$field;)+ }
        }
    };
}
float_state!(
    Health,
    CharacterHealth,
    hit_points,
    max_hit_points,
    invisibility_timer,
    grip_damage,
    fire_damage,
    poison_damage,
    healing,
    bleed_damage
);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Shield {
    kind: u8,
    #[serde(with = "spell_repr")]
    spell: Spell,
    health: f32,
    ice_magnitude: f32,
    earth_magnitude: f32,
    max_health: f32,
    magnitude: f32,
    noise_offsets: [[f32; 2]; 3],
    time_sensitive: bool,
    time_since_damage: f32,
}
impl Shield {
    fn capture(s: &SelfShield) -> Self {
        Self {
            kind: match s.kind {
                SelfShieldType::None => 0,
                SelfShieldType::Shield => 1,
                SelfShieldType::Earth => 2,
                SelfShieldType::Ice => 3,
                SelfShieldType::IcedEarth => 4,
            },
            spell: s.spell,
            health: s.health,
            ice_magnitude: s.ice_magnitude,
            earth_magnitude: s.earth_magnitude,
            max_health: s.max_health,
            magnitude: s.magnitude,
            noise_offsets: s.noise_offsets,
            time_sensitive: s.time_sensitive,
            time_since_damage: s.time_since_damage,
        }
    }
    fn restore(&self) -> SelfShield {
        SelfShield {
            kind: [
                SelfShieldType::None,
                SelfShieldType::Shield,
                SelfShieldType::Earth,
                SelfShieldType::Ice,
                SelfShieldType::IcedEarth,
            ][self.kind as usize],
            spell: self.spell,
            health: self.health,
            ice_magnitude: self.ice_magnitude,
            earth_magnitude: self.earth_magnitude,
            max_health: self.max_health,
            magnitude: self.magnitude,
            noise_offsets: self.noise_offsets,
            time_sensitive: self.time_sensitive,
            time_since_damage: self.time_since_damage,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Item {
    slot: i32,
    name: String,
    attached: bool,
    cooldown: f32,
    special_cooldown: f32,
    #[serde(with = "spell_repr::list")]
    spells: Vec<Spell>,
    gun_clip: i32,
    gun_timer: f32,
    gun_accuracy: f32,
    gun_tracer_count: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Aura {
    resistance: (u32, f32, f32, bool),
    ttl: f32,
    self_casted: bool,
    self_only: bool,
    radius: f32,
    color: [f32; 3],
    decal_cell: u8,
    id: u32,
    period: f32,
    rotation: f32,
    alpha: f32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Buff {
    resistance: (u32, f32, f32, bool),
    ttl: f32,
    self_casted: bool,
    color: [f32; 3],
    id: u32,
}
fn resistance(r: sim::resistance::Resistance) -> (u32, f32, f32, bool) {
    (r.against.0, r.multiplier, r.modifier, r.status_resistance)
}
fn restore_resistance(r: (u32, f32, f32, bool)) -> sim::resistance::Resistance {
    sim::resistance::Resistance {
        against: sim::spells::Elements(r.0),
        multiplier: r.1,
        modifier: r.2,
        status_resistance: r.3,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedWizard {
    version: u32,
    pub loadout: Loadout,
    pub classic: bool,
    #[serde(default = "white")]
    pub robe_colour: [f32; 3],
    health: Health,
    shield: Shield,
    dead: bool,
    overkilled: bool,
    statuses: Vec<(u16, f32, f32, f32, bool)>,
    status_mask: u16,
    dry_timer: f32,
    #[serde(with = "spell_repr::list")]
    queue: Vec<Spell>,
    #[serde(with = "spell_repr")]
    prepared: Spell,
    items: Vec<Item>,
    auras: Vec<Aura>,
    buffs: Vec<Buff>,
    bubble: bool,
    haste: Option<(f32, Option<f32>)>,
    levitation: [f32; 2],
    regeneration: [f32; 2],
    accumulation_timers: [f32; 4],
    time_since_status_damage: f32,
    unlocked_magicks: u64,
}
impl SavedWizard {
    pub fn capture(world: &WizardWorld, classic: bool) -> Self {
        let avatar = world.avatar();
        let c = &avatar.character;
        Self {
            version: 1,
            loadout: world.equipped_loadout(),
            classic,
            robe_colour: world.robe_colour,
            health: Health::capture(&c.combat.health),
            shield: Shield::capture(&c.combat.health.shield),
            dead: c.combat.dead,
            overkilled: c.combat.overkilled,
            statuses: c
                .combat
                .statuses
                .slots()
                .iter()
                .map(|s| {
                    (
                        s.kind as u16,
                        s.magnitude,
                        s.dps,
                        s.damage_rest,
                        s.effect_stops,
                    )
                })
                .collect(),
            status_mask: c.combat.statuses.current_mask,
            dry_timer: c.combat.statuses.dry_timer,
            queue: avatar.conjure.queue.clone(),
            prepared: avatar.conjure.prepared,
            items: avatar
                .items
                .iter()
                .map(|(slot, i)| Item {
                    slot: *slot,
                    name: i.definition.name.clone(),
                    attached: i.attached,
                    cooldown: i.cooldown,
                    special_cooldown: i.special_cooldown,
                    spells: i.spells.queue().into(),
                    gun_clip: i.gun.clip,
                    gun_timer: i.gun.timer,
                    gun_accuracy: i.gun.accuracy,
                    gun_tracer_count: i.gun.tracer_count,
                })
                .collect(),
            auras: c
                .auras
                .iter()
                .map(|a| Aura {
                    resistance: resistance(a.resistance),
                    ttl: a.ttl,
                    self_casted: a.self_casted,
                    self_only: a.self_only,
                    radius: a.visual.radius,
                    color: a.visual.color,
                    decal_cell: a.visual.decal_cell,
                    id: a.visual.buff_id,
                    period: a.visual.period,
                    rotation: a.visual.rotation,
                    alpha: a.visual.alpha,
                })
                .collect(),
            buffs: c
                .buffs
                .list
                .iter()
                .map(|b| Buff {
                    resistance: resistance(b.resistance),
                    ttl: b.ttl,
                    self_casted: b.self_casted,
                    color: b.color,
                    id: b.id,
                })
                .collect(),
            bubble: c.bubble,
            haste: world
                .session
                .world
                .ability_effects
                .effects
                .iter()
                .find_map(|e| match e {
                    AbilityEffect::Haste(h) if h.owner == avatar.handle => {
                        Some((h.ttl, h.custom_ttl))
                    }
                    _ => None,
                }),
            levitation: [c.levitation.timer, c.levitation.free_fall],
            regeneration: [
                c.damage_bookkeeping.regen_timer,
                c.damage_bookkeeping.total_regen,
            ],
            accumulation_timers: c.damage_bookkeeping.accumulation_timers,
            time_since_status_damage: c.damage_bookkeeping.time_since_status_damage,
            unlocked_magicks: world.session.unlocked_magicks[0],
        }
    }

    pub fn dead(&self) -> bool {
        self.dead
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err(format!("unsupported wizard save version {}", self.version));
        }
        if self
            .robe_colour
            .iter()
            .any(|c| !c.is_finite() || !(0. ..=1.).contains(c))
            || self.health.max_hit_points <= 0.
            || self.shield.kind > 4
            || self.status_mask & !0x1ff != 0
        {
            return Err("invalid wizard health/shield/status state".into());
        }
        let valid_spell = |s: &Spell| s.element.0 <= 0xffff && s.mag.iter().all(|v| v.is_finite());
        if self.queue.len() > 5
            || !self.queue.iter().all(valid_spell)
            || !valid_spell(&self.prepared)
            || !valid_spell(&self.shield.spell)
            || self.statuses.len() != 9
            || self.items.len() > 32
            || self.auras.len() > 128
            || self.buffs.len() > 256
        {
            return Err("invalid wizard spell storage".into());
        }
        let mut kinds = std::collections::HashSet::new();
        for s in &self.statuses {
            if s.0 != 0 && (status(s.0).is_none() || !kinds.insert(s.0)) {
                return Err("invalid or duplicate wizard status".into());
            }
        }
        let mut slots = std::collections::HashSet::new();
        for item in &self.items {
            if !slots.insert(item.slot)
                || item.spells.len() > 5
                || !item.spells.iter().all(valid_spell)
            {
                return Err("invalid or duplicate wizard equipment slot".into());
            }
        }
        for (slot, selected) in [
            (0, self.loadout.weapon.as_ref()),
            (1, self.loadout.staff.as_ref()),
        ] {
            if !self
                .items
                .iter()
                .any(|i| i.slot == slot && Some(&i.name) == selected)
            {
                return Err("wizard save loadout disagrees with equipment".into());
            }
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<String, String> {
        self.validate()?;
        let text = serde_json::to_string(self).map_err(|e| e.to_string())?;
        // JSON serializes non-finite floats as null. Reject them, including
        // nullable timers, rather than silently altering a player's state.
        let checked: Self = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if checked != *self {
            return Err("non-finite wizard save state".into());
        }
        Ok(text)
    }
    pub fn decode(text: &str) -> Result<Self, String> {
        let saved: Self = serde_json::from_str(text).map_err(|e| format!("wizard save: {e}"))?;
        saved.encode()?;
        Ok(saved)
    }

    /// Apply only after constructing the destination world's loadout. No old
    /// NPC handles, decal/particle references or VM object ids cross this boundary.
    pub fn restore(&self, world: &mut WizardWorld) -> Result<(), String> {
        self.encode()?;
        if world.equipped_loadout() != self.loadout {
            return Err("wizard restore requires its saved loadout".into());
        }
        // Check every item before mutating anything.
        if world.avatar().items.len() != self.items.len()
            || self.items.iter().any(|saved| {
                !world
                    .avatar()
                    .items
                    .iter()
                    .any(|(slot, i)| *slot == saved.slot && i.definition.name == saved.name)
            })
        {
            return Err("wizard saved equipment no longer matches installed content".into());
        }
        world.robe_colour = self.robe_colour;
        let avatar = world.avatar_mut();
        let c = &mut avatar.character;
        self.health.restore(&mut c.combat.health);
        c.combat.health.shield = self.shield.restore();
        c.combat.dead = self.dead;
        c.combat.overkilled = self.overkilled;
        c.combat.statuses = Default::default();
        for &(kind, magnitude, dps, damage_rest, effect_stops) in &self.statuses {
            if let Some(kind) = status(kind) {
                c.combat
                    .statuses
                    .set_effect(StatusEffect {
                        kind,
                        magnitude,
                        dps,
                        damage_rest,
                        effect_stops,
                    })
                    .expect("validated status kind");
            }
        }
        c.combat.statuses.current_mask = self.status_mask;
        c.combat.statuses.dry_timer = self.dry_timer;
        c.damage_bookkeeping.regen_timer = self.regeneration[0];
        c.damage_bookkeeping.total_regen = self.regeneration[1];
        c.damage_bookkeeping.accumulation_timers = self.accumulation_timers;
        c.damage_bookkeeping.time_since_status_damage = self.time_since_status_damage;
        c.bubble = self.bubble;
        c.levitation.timer = self.levitation[0];
        c.levitation.free_fall = self.levitation[1];
        let mut ids = std::collections::HashMap::new();
        let mut id = |old| *ids.entry(old).or_insert_with(runtime::buffs::next_buff_id);
        c.auras = self
            .auras
            .iter()
            .map(|a| {
                let mut visual = runtime::buffs::AuraVisual::new(a.radius, a.color);
                visual.buff_id = id(a.id);
                visual.period = a.period;
                visual.rotation = a.rotation;
                visual.alpha = a.alpha;
                visual.decal_cell = a.decal_cell;
                runtime::character::ActiveAura {
                    resistance: restore_resistance(a.resistance),
                    ttl: a.ttl,
                    self_casted: a.self_casted,
                    self_only: a.self_only,
                    visual,
                }
            })
            .collect();
        c.buffs.list = self
            .buffs
            .iter()
            .map(|b| runtime::buffs::ActiveBuff {
                id: id(b.id),
                resistance: restore_resistance(b.resistance),
                ttl: b.ttl,
                self_casted: b.self_casted,
                color: b.color,
                decal: None,
            })
            .collect();
        avatar.conjure.queue = self.queue.clone();
        avatar.conjure.prepared = self.prepared;
        for (slot, item) in &mut avatar.items {
            let saved = self.items.iter().find(|s| s.slot == *slot).unwrap();
            item.attached = saved.attached;
            item.cooldown = saved.cooldown;
            item.special_cooldown = saved.special_cooldown;
            item.spells = sim::weapon_spells::WeaponSpells::from_stored_queue(saved.spells.clone());
            item.gun.clip = saved.gun_clip;
            item.gun.timer = saved.gun_timer;
            item.gun.accuracy = saved.gun_accuracy;
            item.gun.tracer_count = saved.gun_tracer_count;
        }
        let owner = avatar.handle;
        world
            .session
            .world
            .ability_effects
            .effects
            .retain(|e| !matches!(e, AbilityEffect::Haste(h) if h.owner == owner));
        if let Some((ttl, custom_ttl)) = self.haste {
            world
                .session
                .world
                .ability_effects
                .effects
                .push(AbilityEffect::Haste(Haste {
                    owner,
                    ttl,
                    custom_ttl,
                }));
        }
        world.session.unlocked_magicks[0] = self.unlocked_magicks;
        world.session.world.unlocked_magick_mask = self.unlocked_magicks;
        Ok(())
    }
}
fn status(kind: u16) -> Option<Status> {
    [
        Status::Burning,
        Status::Wet,
        Status::Frozen,
        Status::Cold,
        Status::Poisoned,
        Status::Healing,
        Status::Greased,
        Status::Steamed,
        Status::Bleeding,
    ]
    .into_iter()
    .find(|s| *s as u16 == kind)
}
