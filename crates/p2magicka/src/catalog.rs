//! Loadout choices decoded from the user's installed content.
use crate::{Loadout, WizardWorld, magicka_content::Content, magicka_runtime::play_content};
use std::path::Path;

#[derive(Default, Debug)]
pub struct Catalog {
    pub robes: Vec<String>,
    pub staffs: Vec<String>,
    pub weapons: Vec<String>,
}

impl Catalog {
    pub fn load(content: &Path) -> Result<Self, String> {
        let mut catalog = Self::default();
        let characters = play_content::resolve(content, Path::new("Data/Characters"))
            .ok_or("Magicka character directory missing")?;
        for entry in std::fs::read_dir(characters).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if !path
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("xnb"))
                || !name.to_ascii_lowercase().starts_with("wizard")
            {
                continue;
            }
            let template = play_content::load_character(content, name)
                .map_err(|e| format!("{name}: {e:?}"))?;
            if [0, 1]
                .iter()
                .all(|slot| template.equipment.iter().any(|(s, _)| s == slot))
            {
                catalog.robes.push(name.into());
            }
        }
        let items = play_content::resolve(content, Path::new("Data/items/Wizard"))
            .ok_or("Magicka equipment directory missing")?;
        for entry in std::fs::read_dir(items).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if !path
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("xnb"))
            {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            let xnb = magicka_xnb::xnb::parse(&bytes).map_err(|e| format!("{name}: {e:?}"))?;
            let parsed = crate::magicka_content::parse_primary(&xnb)
                .ok_or_else(|| format!("{name}: empty item"))?
                .map_err(|e| format!("{name}: {e:?}"))?;
            let Content::Item(item) = parsed.content else {
                continue;
            };
            if item.model.is_none() {
                continue;
            }
            if item.weapon_class == 18 {
                catalog.staffs.push(name.into());
            } else {
                catalog.weapons.push(name.into());
            }
        }
        catalog.robes.sort_by_key(|s| s.to_ascii_lowercase());
        catalog.staffs.sort_by_key(|s| s.to_ascii_lowercase());
        catalog.weapons.sort_by_key(|s| s.to_ascii_lowercase());
        Ok(catalog)
    }
}

impl WizardWorld {
    /// Actual equipped names, including robe defaults and items changed by spells.
    pub fn equipped_loadout(&self) -> Loadout {
        let name = |slot| {
            self.avatar()
                .items
                .iter()
                .find(|(s, _)| *s == slot)
                .map(|(_, i)| i.definition.name.clone())
        };
        Loadout {
            robe: self
                .session
                .player_robe(0)
                .unwrap_or(&self.loadout.robe)
                .into(),
            staff: name(1),
            weapon: name(0),
        }
    }

    /// Host loadout selection. Switching robes preserves the fraction of health;
    /// switching gear cannot cleanse statuses, heal or refill existing items.
    pub fn equip(&mut self, loadout: Loadout) -> Result<(), String> {
        if self.avatar().dead() {
            return Err("Cannot change equipment while dead".into());
        }
        let current = self.equipped_loadout();
        let staff = loadout
            .staff
            .as_ref()
            .or(current.staff.as_ref())
            .ok_or("Missing staff")?;
        let weapon = loadout
            .weapon
            .as_ref()
            .or(current.weapon.as_ref())
            .ok_or("Missing melee weapon")?;
        if current.robe == loadout.robe
            && current.staff.as_ref() == Some(staff)
            && current.weapon.as_ref() == Some(weapon)
        {
            return Ok(());
        }
        let old = self.avatar().character.clone();
        let fraction = old.combat.health.hit_points / old.combat.health.max_hit_points.max(1.);
        let mut old_items = std::mem::take(&mut self.avatar_mut().items);
        if let Err(error) = self
            .session
            .set_player_loadout(0, &loadout.robe, staff, weapon)
        {
            self.avatar_mut().items = old_items;
            return Err(format!("{error:?}"));
        }
        let avatar = self.avatar_mut();
        for (slot, item) in &mut avatar.items {
            if let Some(index) = old_items
                .iter()
                .position(|(s, old)| s == slot && old.definition.name == item.definition.name)
            {
                *item = old_items.remove(index).1;
            }
        }
        let maximum = avatar.character.combat.health.max_hit_points;
        // The new template owns base stats/resistances; live combat state stays.
        avatar.character.combat = old.combat;
        avatar.character.combat.health.max_hit_points = maximum;
        avatar.character.combat.health.hit_points = fraction * maximum;
        avatar.character.auras = old.auras;
        avatar.character.nonvisual_auras = old.nonvisual_auras;
        avatar.character.generic_auras = old.generic_auras;
        avatar.character.buffs = old.buffs;
        avatar.character.halo = old.halo;
        avatar.character.bubble = old.bubble;
        self.loadout = self.equipped_loadout();
        Ok(())
    }
}
