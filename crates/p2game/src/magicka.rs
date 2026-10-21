//! Magicka gameplay host. The rewrite runs on Postal collision and mirrors
//! its combat results into the original pawn scripts (damage, death, police AI).
use crate::{ui::UiInput, window::Sim};
use bevy::{input::mouse::AccumulatedMouseScroll, prelude::*};
use p2magicka::{Loadout, PlayerInput, WizardWorld, magicka_runtime::avatar::AvatarEvent};
use std::{collections::HashMap, path::PathBuf};
use uscript::vm::{ObjId, Value};

pub(crate) struct MagickaPlugin;
impl Plugin for MagickaPlugin {
    fn build(&self, app: &mut App) {
        if !enabled() {
            return;
        }
        app.insert_non_send(MagicState {
            classic: option("--magicka-camera").is_some_and(|c| c.eq_ignore_ascii_case("classic")),
            ..default()
        })
        .add_systems(Startup, hud_setup)
        .add_systems(Update, visuals.after(tick).before(crate::ui::follow_player))
        .add_systems(
            Update,
            tick.after(crate::ui::ui_frame)
                .before(crate::ui::follow_player),
        );
    }
}
pub(crate) fn enabled() -> bool {
    std::env::args().any(|a| a == "--magicka")
}
fn option(name: &str) -> Option<String> {
    std::env::args().skip_while(|a| a != name).nth(1)
}
fn content_dir() -> PathBuf {
    option("--magicka-content")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("MAGICKA_CONTENT").map(PathBuf::from))
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../re-lab-starter/target/Magicka/Content")
        })
}
#[derive(Default)]
pub(crate) struct MagicState {
    pub world: Option<WizardWorld>,
    map: String,
    error: Option<String>,
    mirrors: HashMap<ObjId, Mirror>,
    pub(crate) classic: bool,
    pub aim: Vec3,
    pub notices: String,
}
struct Mirror {
    handle: u16,
    health: f32,
    remainder: f32,
}
#[derive(Component)]
struct MagicHud;
fn hud_setup(mut commands: Commands) {
    commands.spawn((
        MagicHud,
        Text::new("Magicka: waiting for player…"),
        TextFont {
            font_size: bevy::text::FontSize::Px(17.),
            ..default()
        },
        TextColor(Color::srgb(0.95, 0.85, 0.55)),
        TextShadow::default(),
        BackgroundColor(Color::srgba(0.02, 0.01, 0.05, 0.8)),
        Node {
            position_type: PositionType::Absolute,
            bottom: px(16.),
            left: px(16.),
            padding: UiRect::all(px(10.)),
            ..default()
        },
        GlobalZIndex(i32::MAX - 2),
    ));
}

/// Consume wizard controls before Postal executes bindings. UI menus still
/// receive unmodified buttons. Releases are passed through to clear native
/// keys held while opening a menu or switching cameras.
pub(crate) fn filter_events(
    events: &mut Vec<p2input::bevy_map::InputEvent>,
    captured: bool,
    classic: bool,
) {
    if !enabled() || captured {
        return;
    }
    use p2input::keys::{Action, Key};
    events.retain(|e| {
        e.action == Action::Release
            || !(matches!(
                e.key,
                Key::LEFT_MOUSE
                    | Key::RIGHT_MOUSE
                    | Key::MIDDLE_MOUSE
                    | Key::MOUSE_WHEEL_UP
                    | Key::MOUSE_WHEEL_DOWN
            ) || (b'1'..=b'8').contains(&e.key.0)
                || matches!(e.key.0, b'Q' | b'R')
                || (classic
                    && (matches!(e.key, Key::MOUSE_X | Key::MOUSE_Y | Key::SPACE)
                        || b"QWERASDF".contains(&e.key.0))))
    });
}

#[allow(clippy::too_many_arguments)]
fn tick(
    mut state: NonSendMut<MagicState>,
    mut sim: NonSendMut<Sim>,
    ui: Res<UiInput>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    scroll: Res<AccumulatedMouseScroll>,
    time: Res<Time>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    cameras: Query<(&Camera, &GlobalTransform), With<crate::window::FlyCam>>,
    mut hud: Query<&mut Text, With<MagicHud>>,
) {
    let state = &mut *state;
    let Some(pc) = sim.game.player_controller else {
        return;
    };
    let vm = &mut sim.game.world.vm;
    let Some(pawn) = vm.get_prop(pc, "Pawn").and_then(Value::as_obj) else {
        return;
    };
    if !ui.game_look || ui.captured || sim.paused {
        return;
    }
    let level_key = format!("{}:{}", sim.game.level.map.name, sim.game.level_serial);
    if state.map != level_key {
        state.map = level_key;
        state.error = None;
        state.mirrors.clear();
        // Rebuild all collision and actor handles on travel. Gear persistence
        // is handled by the host loadout, independent of Postal package ids.
        if let Some(collision) = p2game::physics::level_collision() {
            let mut tris = Vec::new();
            if let Some(bounds) = collision.bounds() {
                collision.for_tris_in(&bounds, |t| tris.push(t.map(|p| p.to_array())));
            }
            let loadout = Loadout {
                robe: option("--robe").unwrap_or_else(|| "Wizard".into()),
                staff: option("--staff"),
                weapon: option("--melee"),
            };
            match WizardWorld::new(&content_dir(), p2magicka::postal_level(tris), loadout) {
                Ok(world) => {
                    println!(
                        "MAGICKA: hosted wizard and Postal collision for {}",
                        state.map
                    );
                    state.world = Some(world);
                }
                Err(error) => {
                    eprintln!("MAGICKA: {error}");
                    state.error = Some(error);
                    state.world = None;
                }
            }
        }
    }
    if let Some(error) = &state.error {
        for mut text in &mut hud {
            **text = format!("Magicka could not load: {error}");
        }
        return;
    }
    if keys.just_pressed(KeyCode::F6) {
        state.classic = !state.classic;
    }
    let Some(wizard) = state.world.as_mut() else {
        return;
    };
    let vm = &mut sim.game.world.vm;
    let position = vm.actor_loc(pawn);
    let rotation = vm
        .get_prop(pc, "Rotation")
        .map(Value::as_rot)
        .unwrap_or_default();
    let yaw = rotation[1] as f32 * std::f32::consts::TAU / 65536.;
    let pitch = rotation[0] as i16 as f32 * std::f32::consts::TAU / 65536.;
    let facing = Vec3::new(
        yaw.cos() * pitch.cos(),
        yaw.sin() * pitch.cos(),
        pitch.sin(),
    );
    let eye = Vec3::from(position)
        + Vec3::Z
            * vm.get_prop(pawn, "EyeHeight")
                .map(Value::as_float)
                .unwrap_or(60.);
    let mut aim = eye + facing * 5000.;
    if state.classic {
        if let (Ok(win), Ok((camera, transform))) = (windows.single(), cameras.single()) {
            if let Some(cursor) = win.cursor_position() {
                if let Ok(ray) = camera.viewport_to_world(transform, cursor) {
                    // Aim at the ground plane when looking from above; the
                    // collision trace below refines it onto actual geometry.
                    let plane = p2view::scene::to_bevy(position);
                    if let Some(t) = ray.intersect_plane(plane, InfinitePlane3d::new(Vec3::Y)) {
                        let p = ray.get_point(t);
                        aim = Vec3::new(p.x, p.z, p.y) * p2view::scene::UU_PER_M;
                    }
                }
            }
        }
    } else if let Some(collision) = p2game::physics::level_collision() {
        if let Some(hit) = collision.line(eye, aim) {
            aim = hit.location;
        }
    }
    state.aim = aim;
    let aim_dir = (aim - Vec3::from(position)).normalize_or_zero();
    wizard.place(position, aim_dir.to_array());
    let body_index = wizard.avatar().body;
    // Match capsule centres: both engines use centre positions, with their
    // respective radius/length determining the feet offset.
    let postal_half = vm
        .get_prop(pawn, "CollisionHeight")
        .map(Value::as_float)
        .unwrap_or(40.);
    let mage_half = -wizard.body().height_offset() * p2magicka::UNREAL_UNITS_PER_METRE;
    wizard.world.physics.bodies[body_index].position[1] +=
        (mage_half - postal_half) / p2magicka::UNREAL_UNITS_PER_METRE;
    let body_before = wizard.body().position;
    // Native weapon selection and firing are suppressed while the wizard owns combat.
    vm.set_prop(pc, "bFire", Value::Int(0));
    vm.set_prop(pc, "bAltFire", Value::Int(0));
    // Host live Postal characters in the rewrite's combat world.
    if let Ok(pawn_class) = vm.find_class_by_name("Pawn") {
        for actor in vm.live_actors() {
            if actor == pawn || !vm.is_child_of(vm.objects[actor].class, pawn_class) {
                continue;
            }
            let hp = vm
                .get_prop(actor, "Health")
                .map(Value::as_float)
                .unwrap_or(0.);
            if hp <= 0. {
                continue;
            }
            let pos = vm.actor_loc(actor);
            if !state.mirrors.contains_key(&actor) {
                let radius = vm
                    .get_prop(actor, "CollisionRadius")
                    .map(Value::as_float)
                    .unwrap_or(24.);
                let height = vm
                    .get_prop(actor, "CollisionHeight")
                    .map(Value::as_float)
                    .unwrap_or(40.);
                match wizard.attach_pawn(pos, hp * 10., radius, height) {
                    Ok(handle) => {
                        state.mirrors.insert(
                            actor,
                            Mirror {
                                handle,
                                health: hp,
                                remainder: 0.,
                            },
                        );
                    }
                    Err(error) => {
                        eprintln!("MAGICKA pawn: {error}");
                        continue;
                    }
                }
            }
            let mirror = &mut state.mirrors.get_mut(&actor).unwrap();
            if let Some(npc) = wizard.world.npcs.get_mut(&mirror.handle) {
                npc.character.combat.health.hit_points += (hp - mirror.health) * 10.;
                let body = &mut wizard.world.physics.bodies[npc.body];
                body.move_to(p2magicka::to_magicka(pos), body.orientation);
                body.velocity = [0.; 3];
            }
            mirror.health = hp;
        }
    }
    let dt = time.delta_secs().clamp(0., 0.05);
    let element_keys = if state.classic {
        [
            KeyCode::KeyQ,
            KeyCode::KeyW,
            KeyCode::KeyE,
            KeyCode::KeyR,
            KeyCode::KeyA,
            KeyCode::KeyS,
            KeyCode::KeyD,
            KeyCode::KeyF,
        ]
    } else {
        [
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
            KeyCode::Digit5,
            KeyCode::Digit6,
            KeyCode::Digit7,
            KeyCode::Digit8,
        ]
    };
    let input = PlayerInput {
        // First-person retains WASD; numbers are the same ordered eight
        // elements as classic Q W E R A S D F.
        elements: element_keys.map(|k| keys.pressed(k)),
        move_down: state.classic && mouse.pressed(MouseButton::Left),
        cast_down: mouse.pressed(if state.classic {
            MouseButton::Right
        } else {
            MouseButton::Left
        }),
        cast_self_down: mouse.pressed(MouseButton::Middle),
        shift_down: keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight),
        pad_movement: (!state.classic).then_some([0., 0.]),
        pad_attack: !state.classic && mouse.pressed(MouseButton::Right),
        boost_down: keys.pressed(if state.classic {
            KeyCode::Space
        } else {
            KeyCode::KeyQ
        }),
        magick_prev: scroll.delta.y > 0.,
        magick_next: scroll.delta.y < 0.,
        ..default()
    };
    wizard.input(input, aim.to_array(), dt);
    match wizard.step(dt) {
        Ok(events) => {
            for event in events {
                if let AvatarEvent::SpellCast {
                    spell, cast_type, ..
                } = event
                {
                    state.notices = format!("{:?}: {:?}", cast_type, spell.element);
                    println!("MAGICKA CAST {:?} {:?}", cast_type, spell);
                }
            }
        }
        Err(error) => {
            state.error = Some(error);
            return;
        }
    }
    // Robe speed, queue slowdown and Haste drive first-person walking too.
    let speed = if state.classic {
        0.
    } else {
        wizard.body().max_speed
            * wizard.body().speed_multiplier()
            * p2magicka::UNREAL_UNITS_PER_METRE
    };
    vm.set_prop(pawn, "GroundSpeed", Value::Float(speed));
    // Magicka teleport, push and levitation results are resolved by Postal's
    // collision on the next movement frame.
    let delta = Vec3::from(p2magicka::to_postal(std::array::from_fn(|i| {
        wizard.body().position[i] - body_before[i]
    })));
    if delta.length_squared() > 1. {
        vm.set_prop(
            pawn,
            "Location",
            Value::Vector((Vec3::from(position) + delta).to_array()),
        );
    }
    let damage_type = Value::Class(vm.find_class("Engine", "DamageType").ok());
    for (&actor, mirror) in &mut state.mirrors {
        if vm.objects.get(actor).is_none_or(|o| o.deleted) {
            continue;
        }
        let Some(npc) = wizard.world.npcs.get(&mirror.handle) else {
            continue;
        };
        let new_health = npc.character.combat.health.hit_points / 10.;
        let damage = mirror.health - new_health + mirror.remainder;
        if damage >= 1. {
            let amount = damage.floor() as i32;
            vm.fire_event(
                actor,
                "TakeDamage",
                vec![
                    Value::Int(amount),
                    Value::Object(Some(pawn)),
                    Value::Vector(vm.actor_loc(actor)),
                    Value::Vector([0.; 3]),
                    damage_type.clone(),
                ],
            );
            mirror.remainder = damage - amount as f32;
        } else if damage <= -1. {
            vm.set_prop(actor, "Health", Value::Int(new_health as i32));
            mirror.remainder = 0.;
        } else {
            mirror.remainder = damage;
        }
        mirror.health = vm
            .get_prop(actor, "Health")
            .map(Value::as_float)
            .unwrap_or(0.);
        // The native TakeDamage may apply armour or script immunity. Keep
        // subsequent damage deltas relative to that actual result.
        if let Some(npc) = wizard.world.npcs.get_mut(&mirror.handle) {
            npc.character.combat.health.hit_points = mirror.health * 10.;
        }
    }
    let health = &wizard.avatar().character.combat.health;
    let queue = wizard
        .avatar()
        .conjure
        .queue
        .iter()
        .map(|s| format!("{:?}", s.element))
        .collect::<Vec<_>>()
        .join(" · ");
    let gear = wizard
        .avatar()
        .items
        .iter()
        .map(|(_, i)| i.definition.name.as_str())
        .collect::<Vec<_>>()
        .join(" / ");
    let controls = if state.classic {
        "Q W E R / A S D F: Water Life Shield Cold / Lightning Arcane Earth Fire\nLMB move · Shift+LMB melee · RMB cast · Shift+RMB area · MMB self / staff · Space magick · F6 camera"
    } else {
        "1..8: Water Life Shield Cold Lightning Arcane Earth Fire\nWASD move · LMB cast · Shift+LMB area · MMB self / staff · RMB melee · Q magick · F6 camera"
    };
    for mut text in &mut hud {
        **text = format!(
            "{}  HP {:.0}/{:.0}  {}\n{}\n{}\n{}\n{}",
            wizard.loadout.robe,
            health.hit_points,
            health.max_hit_points,
            if state.classic {
                "Classic camera"
            } else {
                "First person"
            },
            gear,
            controls,
            queue,
            state.notices
        );
    }
}

pub(crate) fn camera(
    state: Option<NonSend<MagicState>>,
    ui: Res<UiInput>,
    sim: NonSend<Sim>,
    mut cameras: Query<&mut Transform, With<crate::window::FlyCam>>,
) {
    let Some(state) = state else { return };
    if !state.classic || !ui.game_look || ui.captured {
        return;
    }
    let Some(pc) = sim.game.player_controller else {
        return;
    };
    let vm = &sim.game.world.vm;
    let Some(pawn) = vm.get_prop(pc, "Pawn").and_then(Value::as_obj) else {
        return;
    };
    let target = p2view::scene::to_bevy(vm.actor_loc(pawn));
    if let Ok(mut camera) = cameras.single_mut() {
        let desired = target + Vec3::new(0., 600., 712.5) / p2view::scene::UU_PER_M;
        let postal_start = Vec3::new(target.x, target.z, target.y) * p2view::scene::UU_PER_M;
        let postal_end = Vec3::new(desired.x, desired.z, desired.y) * p2view::scene::UU_PER_M;
        let position = p2game::physics::level_collision()
            .and_then(|c| c.line(postal_start, postal_end))
            .map(|h| p2view::scene::to_bevy(h.location.to_array()) + Vec3::Y * 0.4)
            .unwrap_or(desired);
        *camera = Transform::from_translation(position).looking_at(target, Vec3::Y);
    }
}

#[derive(Default)]
struct WizardDraws {
    map: String,
    parts: Vec<(p2magicka::visual::Part, Entity, Handle<Mesh>)>,
}
fn visuals(
    mut commands: Commands,
    state: NonSend<MagicState>,
    sim: NonSend<Sim>,
    ui: Res<UiInput>,
    mut draws: Local<WizardDraws>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut visibility: Query<&mut Visibility>,
) {
    let Some(wizard) = &state.world else { return };
    if draws.map != state.map {
        for (_, entity, _) in draws.parts.drain(..) {
            commands.entity(entity).despawn();
        }
        draws.map.clone_from(&state.map);
        match p2magicka::visual::load(wizard) {
            Ok(parts) => {
                println!(
                    "MAGICKA: loaded {} original robe and equipment mesh parts",
                    parts.len()
                );
                for mut part in parts {
                    let texture = part.texture.take().map(|t| {
                        images.add(Image::new(
                            bevy::render::render_resource::Extent3d {
                                width: t.width,
                                height: t.height,
                                depth_or_array_layers: 1,
                            },
                            bevy::render::render_resource::TextureDimension::D2,
                            t.rgba,
                            bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
                            bevy::asset::RenderAssetUsages::default(),
                        ))
                    });
                    let mut mesh = Mesh::new(
                        bevy::mesh::PrimitiveTopology::TriangleList,
                        bevy::asset::RenderAssetUsages::default(),
                    );
                    mesh.insert_attribute(
                        Mesh::ATTRIBUTE_POSITION,
                        part.geometry.positions.clone(),
                    );
                    if let Some(uv) = &part.geometry.uvs {
                        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv.clone());
                    }
                    mesh.insert_indices(bevy::mesh::Indices::U32(part.geometry.indices.clone()));
                    mesh.compute_smooth_normals();
                    let mesh = meshes.add(mesh);
                    let material = materials.add(StandardMaterial {
                        base_color: Color::srgb(part.color[0], part.color[1], part.color[2]),
                        base_color_texture: texture,
                        perceptual_roughness: 0.85,
                        cull_mode: None,
                        alpha_mode: AlphaMode::Mask(0.3),
                        ..default()
                    });
                    let entity = commands
                        .spawn((
                            Mesh3d(mesh.clone()),
                            MeshMaterial3d(material),
                            Transform::IDENTITY,
                            bevy::camera::visibility::NoFrustumCulling,
                        ))
                        .id();
                    draws.parts.push((part, entity, mesh));
                }
            }
            Err(error) => eprintln!("MAGICKA visual: {error}"),
        }
    }
    let Some((skin, items)) = p2magicka::visual::pose(wizard) else {
        return;
    };
    let eye_and_forward = sim.game.player_controller.and_then(|pc| {
        let vm = &sim.game.world.vm;
        let pawn = vm.get_prop(pc, "Pawn")?.as_obj()?;
        let eye = p2view::scene::to_bevy(vm.actor_loc(pawn))
            + Vec3::Y
                * vm.get_prop(pawn, "EyeHeight")
                    .map(Value::as_float)
                    .unwrap_or(60.)
                / p2view::scene::UU_PER_M;
        let aim = p2view::scene::to_bevy(state.aim.to_array());
        Some((eye, (aim - eye).normalize_or(Vec3::NEG_Z)))
    });
    for (part, entity, handle) in &draws.parts {
        if let Ok(mut v) = visibility.get_mut(*entity) {
            *v = if ui.player_view && (state.classic || part.slot.is_some()) {
                Visibility::Visible
            } else {
                Visibility::Hidden
            };
        }
        let mut positions = p2magicka::visual::positions(part, &skin, &items);
        if !state.classic {
            if let (Some(slot), Some((eye, forward))) = (part.slot, eye_and_forward) {
                if let Some((_, transform)) = items.iter().find(|(s, _)| *s == slot) {
                    let origin = Vec3::new(transform[12], transform[13], transform[14]);
                    let right = forward.cross(Vec3::Y).normalize_or(Vec3::X);
                    let destination =
                        eye + forward * 0.61 + right * if slot == 1 { -0.34 } else { 0.34 }
                            - Vec3::Y * 0.53;
                    let offset = destination - origin;
                    for p in &mut positions {
                        *p = (Vec3::from(*p) + offset).to_array();
                    }
                }
            }
        }
        if let Some(mut mesh) = meshes.get_mut(handle) {
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
            mesh.compute_smooth_normals();
        }
    }
}
