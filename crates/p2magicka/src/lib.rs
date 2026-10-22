//! Adapts the existing Magicka rewrite to Postal 2. Game rules remain in
//! magicka-runtime; this crate owns coordinates, content and host integration.
pub mod visual;
pub mod catalog;
pub mod persistence;

pub use magicka_content;
use magicka_content::level::{LevelModel, NavMesh, TriMesh};
pub use magicka_runtime;
pub use magicka_runtime::avatar::{AvatarEvent, PlayerInput};
use magicka_runtime::{
    avatar::{InputFrame, PlayerAvatar},
    play_content::FsSceneContent,
    play_session::{PlaySession, SessionEvent},
    scene_world::SceneWorld,
};
pub use magicka_sim;
use magicka_sim::character_physics::{CharacterBody, Orientation};
use std::path::{Path, PathBuf};

/// Find user-owned content relative to the running game or working directory.
/// Do not bake the build machine's workspace path into a portable executable.
pub fn discover_content(executable: &Path, working_directory: &Path) -> Option<PathBuf> {
    for anchor in executable.parent().into_iter().chain([working_directory]) {
        for directory in anchor.ancestors() {
            for relative in [
                "re-lab-starter/target/Magicka/Content",
                "target/Magicka/Content",
                "Magicka/Content",
                "Content",
            ] {
                let candidate = directory.join(relative);
                if magicka_runtime::play_content::resolve(
                    &candidate,
                    Path::new("Data/Characters/Wizard.xnb"),
                )
                .is_some_and(|p| p.is_file())
                {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// A script-less Magicka play session on Postal collision: the same
    /// spells, effects, beams and audio requests as a campaign scene.
    pub session: PlaySession,
    pub content: PathBuf,
    pub loadout: Loadout,
}
impl WizardWorld {
    pub fn new(
        content: &Path,
        level: LevelModel,
        loadout: Loadout,
        postal_spawn: [f32; 3],
        postal_facing: [f32; 3],
    ) -> Result<Self, String> {
        // Magicka scenes default to a kill plane at -50 m; Postal maps can lie
        // far below Magicka's origin, so the plane sits under the lowest surface.
        let floor = level.collision_layers[0]
            .vertices
            .iter()
            .map(|v| v[1])
            .fold(0f32, f32::min);
        let facing = [postal_facing[0], postal_facing[2], postal_facing[1]];
        let mut session = PlaySession::hosted(
            content,
            level,
            &loadout.robe,
            to_magicka(postal_spawn),
            facing,
            floor - 50.,
        )
        .map_err(|e| format!("{e:?}"))?;
        if loadout.staff.is_some() || loadout.weapon.is_some() {
            let defaults = session
                .avatar()
                .items
                .iter()
                .map(|(slot, item)| (*slot, item.definition.name.clone()))
                .collect::<Vec<_>>();
            let pick = |slot: i32, chosen: &Option<String>| {
                chosen.clone().or_else(|| {
                    defaults
                        .iter()
                        .find(|(s, _)| *s == slot)
                        .map(|(_, n)| n.clone())
                })
            };
            let (staff, weapon) = (pick(1, &loadout.staff), pick(0, &loadout.weapon));
            session
                .set_player_loadout(
                    0,
                    &loadout.robe,
                    staff.as_deref().unwrap_or("staff_basic"),
                    weapon.as_deref().unwrap_or("weapon_blunt_blade"),
                )
                .map_err(|e| format!("{e:?}"))?;
        }
        session.world.unlocked_magick_mask = magicka_sim::spells::ALL_MAGICKS_MASK;
        for slot in &mut session.unlocked_magicks {
            *slot = magicka_sim::spells::ALL_MAGICKS_MASK;
        }
        Ok(Self {
            session,
            content: content.into(),
            loadout,
        })
    }
    pub fn world(&self) -> &SceneWorld<FsSceneContent> {
        &self.session.world
    }
    pub fn avatar(&self) -> &PlayerAvatar {
        self.session.world.players[0].as_ref().unwrap()
    }
    pub fn avatar_mut(&mut self) -> &mut PlayerAvatar {
        self.session.world.players[0].as_mut().unwrap()
    }
    pub fn body(&self) -> &CharacterBody {
        &self.session.world.physics.bodies[self.avatar().body]
    }
    /// Postal owns ordinary first-person walking. The Magicka body receives
    /// that pose before stepping; teleports and spell impulses remain visible
    /// as the resulting body displacement for the host to resolve.
    pub fn place(&mut self, postal_position: [f32; 3], postal_facing: [f32; 3]) {
        let index = self.avatar().body;
        let f = [postal_facing[0], postal_facing[2], postal_facing[1]];
        let orientation = Orientation::from_forward_up(f, [0., 1., 0.]);
        let body = &mut self.session.world.physics.bodies[index];
        body.move_to(to_magicka(postal_position), orientation);
        body.set_desired_direction(f);
    }
    pub fn input(&mut self, input: PlayerInput, aim: [f32; 3], dt: f32) {
        let world = &mut self.session.world;
        let magicks_allowed = world.unlocked_magick_mask;
        let avatar = world.players[0].as_mut().unwrap();
        avatar.handle_input(
            &mut world.physics.bodies[avatar.body],
            input,
            InputFrame {
                pick: Some(to_magicka(aim)),
                dt,
                play_time: world.play_time,
                magicks_allowed,
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
            .session
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
        let world = &mut self.session.world;
        let npc = world.npcs.get_mut(&handle).unwrap();
        npc.weapons.clear();
        npc.character.definition.faction = 2;
        // Character.Update resets the maximum from the definition each step;
        // the Postal pawn's own health is the template here.
        npc.character.definition.max_hit_points = health;
        npc.character.combat.health.hit_points = health;
        npc.character.combat.health.max_hit_points = health;
        let body = &mut world.physics.bodies[npc.body];
        body.radius = radius / UNREAL_UNITS_PER_METRE;
        body.length = ((half_height - radius) * 2. / UNREAL_UNITS_PER_METRE).max(0.);
        if let Some(e) = world.entities.iter_mut().find(|e| e.handle == handle) {
            e.faction = 2;
            e.original_faction = 2;
            e.hit_points = health;
            e.max_hit_points = health;
        }
        Ok(handle)
    }
    /// Deliver host damage through the player's equipment, resistances,
    /// shields and status processing, exactly like another Magicka entity.
    pub fn receive_damage(
        &mut self,
        damage: magicka_sim::damage::Damage,
        attacker: Option<u16>,
        position: [f32; 3],
    ) -> u16 {
        let world = &mut self.session.world;
        let avatar = world.players[0].as_mut().unwrap();
        avatar.receive_payload(
            &mut world.physics.bodies[avatar.body],
            magicka_sim::damage_delivery::DamagePayload::Single(damage),
            attacker,
            position,
            world.play_time,
        )
    }
    /// PlaySession.Step (world, effects, beams, bolts and audio requests).
    /// Returns this step's avatar events.
    pub fn step(&mut self, dt: f32) -> Result<Vec<AvatarEvent>, String> {
        self.session.audio.clear();
        self.session.events.clear();
        self.session.step(dt.clamp(0., 0.05));
        if let Some(error) = &self.session.script_error {
            return Err(error.clone());
        }
        Ok(self
            .session
            .events
            .iter()
            .filter_map(|e| match e {
                SessionEvent::Avatar(a) => Some(a.clone()),
                _ => None,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn content_discovery_survives_relocation_and_unrelated_working_directory() {
        let root = std::env::temp_dir().join(format!("p2magicka-discovery-{}", std::process::id()));
        let content = root.join("moved workspace/re-lab-starter/target/Magicka/Content");
        let wizard = content.join("data/characters/Wizard.xnb");
        std::fs::create_dir_all(wizard.parent().unwrap()).unwrap();
        std::fs::write(&wizard, []).unwrap();
        let executable = root.join("moved workspace/postal2rust/target/release/p2game");
        let unrelated = root.join("elsewhere");
        assert_eq!(
            discover_content(&executable, &unrelated),
            Some(content.clone())
        );
        // A copied standalone binary can also use a checkout beside the CWD.
        assert_eq!(
            discover_content(
                &unrelated.join("p2game"),
                &root.join("moved workspace/postal2rust")
            ),
            Some(content),
        );
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(discover_content(&executable, &unrelated), None);
    }
    #[test]
    fn coordinates_roundtrip_and_reverse_triangle_winding() {
        let p = [100., -200., 300.];
        assert_eq!(to_postal(to_magicka(p)), p);
        let lv = postal_level([[[0., 0., 0.], [52.5, 0., 0.], [0., 52.5, 0.]]]);
        assert_eq!(lv.collision_layers[0].triangles, vec![[0, 2, 1]]);
        assert_eq!(lv.collision_layers[0].vertices[2], [0., 0., 1.]);
    }
}
