//! Adapts the existing Magicka rewrite to Postal 2. Game rules remain in
//! magicka-runtime; this crate owns coordinates, content and host integration.
pub mod visual;

pub use magicka_content;
use magicka_content::level::{LevelModel, NavMesh, TriMesh};
pub use magicka_runtime;
pub use magicka_runtime::avatar::{AvatarEvent, PlayerInput};
use magicka_runtime::{
    CharacterDefinition,
    animation_controller::AnimationSet,
    avatar::{InputFrame, PlayerAvatar},
    play_content::{self, FsSceneContent},
    scene_loading::custom_hash,
    scene_world::SceneWorld,
};
pub use magicka_sim;
use magicka_sim::character_physics::{CharacterBody, Orientation};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// Host convention: 52.5 Unreal units per metre (Postal renderer convention). Axis exchange reverses
/// handedness, so collision triangle winding must also be reversed.
pub const UNREAL_UNITS_PER_METRE: f32 = 52.5;
pub fn to_magicka(p: [f32; 3]) -> [f32; 3] {
    [
        p[0] / UNREAL_UNITS_PER_METRE,
        p[2] / UNREAL_UNITS_PER_METRE,
        p[1] / UNREAL_UNITS_PER_METRE,
    ]
}
pub fn to_postal(p: [f32; 3]) -> [f32; 3] {
    [
        p[0] * UNREAL_UNITS_PER_METRE,
        p[2] * UNREAL_UNITS_PER_METRE,
        p[1] * UNREAL_UNITS_PER_METRE,
    ]
}

/// Build Magicka collision from Postal's complete collision surface, without
/// retaining any geometry or scripts from a Magicka campaign map.
pub fn postal_level(triangles: impl IntoIterator<Item = [[f32; 3]; 3]>) -> LevelModel {
    let mut mesh = TriMesh::default();
    for tri in triangles {
        let start = mesh.vertices.len() as i32;
        mesh.vertices.extend(tri.map(to_magicka));
        mesh.triangles.push([start, start + 2, start + 1]);
    }
    let mut collision_layers = vec![TriMesh::default(); 10];
    collision_layers[0] = mesh;
    LevelModel {
        model: None,
        animated_parts: vec![],
        lights: vec![],
        effects: vec![],
        physics_entities: vec![],
        liquids: vec![],
        force_fields: vec![],
        collision_layers,
        camera_mesh: None,
        trigger_areas: vec![],
        locators: vec![],
        nav_mesh: NavMesh::default(),
    }
}

#[derive(Clone, Debug)]
pub struct Loadout {
    pub robe: String,
    pub staff: Option<String>,
    pub weapon: Option<String>,
}
impl Default for Loadout {
    fn default() -> Self {
        Self {
            robe: "Wizard".into(),
            staff: None,
            weapon: None,
        }
    }
}

pub struct WizardWorld {
    pub world: SceneWorld<FsSceneContent>,
    pub content: PathBuf,
    pub loadout: Loadout,
}
impl WizardWorld {
    pub fn new(content: &Path, level: LevelModel, loadout: Loadout) -> Result<Self, String> {
        let mut world = SceneWorld::new(
            Arc::new(level),
            FsSceneContent::new(content),
            0x50324d414749434b,
        )?;
        let (body, avatar) = prepare_wizard(content, &loadout)?;
        world.add_player(0, body, avatar);
        world.unlocked_magick_mask = magicka_sim::spells::ALL_MAGICKS_MASK;
        Ok(Self {
            world,
            content: content.into(),
            loadout,
        })
    }
    pub fn avatar(&self) -> &PlayerAvatar {
        self.world.players[0].as_ref().unwrap()
    }
    pub fn avatar_mut(&mut self) -> &mut PlayerAvatar {
        self.world.players[0].as_mut().unwrap()
    }
    pub fn body(&self) -> &CharacterBody {
        &self.world.physics.bodies[self.avatar().body]
    }
    /// Postal owns ordinary first-person walking. The Magicka body receives
    /// that pose before stepping; teleports and spell impulses remain visible
    /// as the resulting body displacement for the host to resolve.
    pub fn place(&mut self, postal_position: [f32; 3], postal_facing: [f32; 3]) {
        let index = self.avatar().body;
        let f = [postal_facing[0], postal_facing[2], postal_facing[1]];
        let orientation = Orientation::from_forward_up(f, [0., 1., 0.]);
        let body = &mut self.world.physics.bodies[index];
        body.move_to(to_magicka(postal_position), orientation);
        body.set_desired_direction(f);
    }
    pub fn input(&mut self, input: PlayerInput, aim: [f32; 3], dt: f32) {
        let world = &mut self.world;
        let avatar = world.players[0].as_mut().unwrap();
        avatar.handle_input(
            &mut world.physics.bodies[avatar.body],
            input,
            InputFrame {
                pick: Some(to_magicka(aim)),
                dt,
                play_time: world.play_time,
                magicks_allowed: world.unlocked_magick_mask,
                ..Default::default()
            },
        );
    }
    /// Host an existing Postal pawn as a Magicka combat character. Postal
    /// retains its AI; this character supplies elemental status and resistance.
    pub fn attach_pawn(
        &mut self,
        position: [f32; 3],
        health: f32,
        radius: f32,
        half_height: f32,
    ) -> Result<u16, String> {
        use magicka_runtime::spawn_action::SpawnOrder;
        let handle = self
            .world
            .summon_npc(
                None,
                "Wizard",
                to_magicka(position),
                [1., 0., 0.],
                magicka_sim::summons::SummonKind::Normal,
                SpawnOrder {
                    order: 0,
                    react_to: 0,
                    reaction: 0,
                    priority_target: 0,
                    priority_ability: 0,
                    trigger: 0,
                    events: &[],
                },
            )
            .ok_or("could not host Postal pawn")?;
        let npc = self.world.npcs.get_mut(&handle).unwrap();
        npc.weapons.clear();
        npc.character.definition.faction = 2;
        npc.character.combat.health.hit_points = health;
        npc.character.combat.health.max_hit_points = health;
        let body = &mut self.world.physics.bodies[npc.body];
        body.radius = radius / UNREAL_UNITS_PER_METRE;
        body.length = ((half_height - radius) * 2. / UNREAL_UNITS_PER_METRE).max(0.);
        if let Some(e) = self.world.entities.iter_mut().find(|e| e.handle == handle) {
            e.faction = 2;
            e.original_faction = 2;
            e.hit_points = health;
            e.max_hit_points = health;
        }
        Ok(handle)
    }
    pub fn step(&mut self, dt: f32) -> Result<Vec<AvatarEvent>, String> {
        let dt = dt.clamp(0., 0.05) * self.world.time_multiplier * self.world.time_modifier;
        self.world.audio_requests.clear();
        self.world.effect_commands.clear();
        self.world
            .step_live(None, dt)
            .map_err(|e| format!("{e:?}"))?;
        Ok(std::mem::take(&mut self.avatar_mut().events))
    }
}

type PlayerFactory = Box<dyn FnOnce(u16, usize) -> PlayerAvatar>;
fn prepare_wizard(
    content: &Path,
    loadout: &Loadout,
) -> Result<(CharacterBody, PlayerFactory), String> {
    let mut template =
        play_content::load_character(content, &loadout.robe).map_err(|e| format!("{e:?}"))?;
    for (slot, name) in [(0, &loadout.weapon), (1, &loadout.staff)] {
        if let Some(name) = name {
            let attachment = template
                .equipment
                .iter_mut()
                .find(|(s, _)| *s == slot)
                .ok_or_else(|| format!("{} has no equipment slot {slot}", loadout.robe))?;
            attachment.1.item = Some(format!("../items/Wizard/{name}"));
        }
    }
    let definition =
        CharacterDefinition::from_content(&template, 1).map_err(|e| format!("{e:?}"))?;
    let rig = play_content::load_rig(content, &template).map_err(|e| format!("{e:?}"))?;
    let animations = Arc::new(AnimationSet::with_weapons(
        &template.animation_sets,
        &play_content::weapon_classes(rig.as_ref()),
        rig.as_ref()
            .map(|r| r.skeleton.clip_durations())
            .unwrap_or_default(),
    ));
    let abilities = play_content::move_abilities(&template);
    let items = play_content::avatar_items(content, &template);
    if ![0, 1]
        .iter()
        .all(|s| items.iter().any(|(slot, _)| slot == s))
    {
        return Err("could not load the wizard's staff and melee weapon".into());
    }
    let template_type = custom_hash(&template.id.to_ascii_lowercase());
    let mut body = CharacterBody::new(
        definition.radius,
        definition.length,
        definition.mass,
        definition.speed,
    );
    body.owner.turn_speed = definition.turn_speed;
    body.owner.hit_points = definition.max_hit_points;
    Ok((
        body,
        Box::new(move |handle, index| {
            let mut avatar = PlayerAvatar::new(
                handle,
                0,
                index,
                definition,
                animations,
                abilities,
                template_type,
            );
            avatar.rig = rig;
            avatar.items = items;
            avatar
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coordinates_roundtrip_and_reverse_triangle_winding() {
        let p = [100., -200., 300.];
        assert_eq!(to_postal(to_magicka(p)), p);
        let lv = postal_level([[[0., 0., 0.], [52.5, 0., 0.], [0., 52.5, 0.]]]);
        assert_eq!(lv.collision_layers[0].triangles, vec![[0, 2, 1]]);
        assert_eq!(lv.collision_layers[0].vertices[2], [0., 0., 1.]);
    }
}
