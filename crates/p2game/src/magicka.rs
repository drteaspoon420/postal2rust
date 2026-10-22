//! Magicka gameplay host. The rewrite runs on Postal collision and mirrors
//! its combat results into the original pawn scripts (damage, death, police AI).
use crate::{ui::UiInput, window::Sim};
use bevy::{input::mouse::AccumulatedMouseScroll, prelude::*};
use p2magicka::{Loadout, PlayerInput, WizardWorld, magicka_runtime::avatar::AvatarEvent};
use p2magicka::persistence::SavedWizard;
use std::{collections::HashMap, path::PathBuf};
use uscript::vm::{ObjId, Value};
mod wardrobe;

// Default Wizard has 1000 HP; Postal's default player has 100. This host
// conversion keeps a gunshot's relative strength while retaining robe stats.
const HEALTH_SCALE: f32 = 10.;

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
        .add_plugins(crate::magicka_fx::MagickaFxPlugin)
        .add_systems(Startup, (hud_setup, wardrobe::setup, persistence_pending))
        .add_systems(Update, wardrobe::draw.after(tick))
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
fn content_dir() -> Result<PathBuf, String> {
    option("--magicka-content")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("MAGICKA_CONTENT").map(PathBuf::from))
        .or_else(|| {
            p2magicka::discover_content(
                &std::env::current_exe().ok()?,
                &std::env::current_dir().ok()?,
            )
        })
        .ok_or_else(|| {
            "Magicka content was not found beside the project. Use --magicka-content /path/to/Magicka/Content or set MAGICKA_CONTENT.".into()
        })
}
#[derive(Default)]
pub(crate) struct MagicState {
    pub world: Option<WizardWorld>,
    map: String,
    pawn: Option<ObjId>,
    published_health: Option<i32>,
    death_delivered: bool,
    error: Option<String>,
    mirrors: HashMap<ObjId, Mirror>,
    pub(crate) classic: bool,
    pub aim: Vec3,
    /// Orb of the staff held in first person (Postal units), drawn last frame.
    /// Spells leave from here, as the original reads the previous frame's orb.
    fp_orb: Option<Vec3>,
    pub notices: String,
    /// Elements of the wizard's latest spell and when it was last active, so
    /// Postal hears which kind of damage a victim took.
    recent_elements: (u32, f32),
    clock: f32,
    wardrobe: wardrobe::Wardrobe,
}
impl MagicState {
    pub(crate) fn loadout_open(&self) -> bool { self.wardrobe.open }
}
fn persistence_pending(mut sim: NonSendMut<Sim>) {
    sim.game.save_extension_errors.insert("magicka".into(), "Wizard is still loading".into());
}
fn publish_wizard(wizard: &WizardWorld, classic: bool, game: &mut p2game::boot::Game) -> Result<(), String> {
    match SavedWizard::capture(wizard, classic).encode() {
        Ok(payload) => {
            game.save_extensions.insert("magicka".into(), payload);
            game.save_extension_errors.remove("magicka");
            Ok(())
        }
        Err(error) => {
            game.save_extension_errors.insert("magicka".into(), error.clone());
            Err(error)
        }
    }
}
struct Mirror {
    handle: u16,
    health: f32,
    remainder: f32,
    /// Native Postal reactions already started for the current Magicka status.
    burning: bool,
    frozen_speed: Option<f32>,
    gibbed: bool,
}
#[derive(Component)]
pub(crate) struct MagicHud;
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
pub(crate) fn tick(
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
    let level_key = format!("{}:{}", sim.game.level.map.name, sim.game.level_serial);
    if state.map != level_key || state.pawn != Some(pawn) {
        let level_changed = state.map != level_key;
        let saved = if level_changed {
            sim.game.save_extensions.get("magicka").map(|text| SavedWizard::decode(text)).transpose()
        } else { Ok(None) };
        state.map = level_key;
        state.error = None;
        state.mirrors.clear();
        state.wardrobe.open = false;
        state.notices.clear();
        state.recent_elements = (0, 0.);
        state.clock = 0.;
        // Rebuild all collision and actor handles on travel. Gear persistence
        // is handled by the host loadout, independent of Postal package ids.
        if let Some(collision) = p2game::physics::level_collision() {
            let mut tris = Vec::new();
            if let Some(bounds) = collision.bounds() {
                collision.for_tris_in(&bounds, |t| tris.push(t.map(|p| p.to_array())));
            }
            let loadout = saved.as_ref().ok().and_then(|s| s.as_ref()).map(|s| s.loadout.clone())
                .or_else(|| (!level_changed).then(|| state.world.as_ref().map(WizardWorld::equipped_loadout)).flatten())
                .unwrap_or_else(|| Loadout {
                robe: option("--robe").unwrap_or_else(|| "Wizard".into()),
                staff: option("--staff"),
                weapon: option("--melee"),
            });
            let vm = &sim.game.world.vm;
            let position = vm.actor_loc(pawn);
            let yaw = vm
                .get_prop(pc, "Rotation")
                .map(Value::as_rot)
                .unwrap_or_default()[1] as f32
                * std::f32::consts::TAU
                / 65536.;
            let native_health = vm.get_prop(pawn, "Health").map(Value::as_float).unwrap_or(100.);
            let loaded_from_save = sim.game.loaded_from_save;
            match saved.and_then(|saved| content_dir().and_then(|content| {
                let mut world = WizardWorld::new(
                    &content,
                    p2magicka::postal_level(tris),
                    loadout,
                    position,
                    [yaw.cos(), yaw.sin(), 0.],
                )?;
                if let Some(saved) = saved {
                    state.classic = saved.classic;
                    // A fresh native pawn after death is a respawn; loading an
                    // actual dead save retains its death state instead.
                    if !saved.dead() || loaded_from_save {
                        saved.restore(&mut world)?;
                        let c = &world.avatar().character.combat;
                        println!("MAGICKA RESTORED {} camera={} hp={} shield={} status={:#x} queue={} {:?}", state.map,
                            if state.classic { "classic" } else { "first-person" },
                            c.health.hit_points, c.health.shield.health, c.statuses.current_mask,
                            world.avatar().conjure.queue.len(), world.equipped_loadout());
                    }
                } else if loaded_from_save && level_changed {
                    let hp = &mut world.avatar_mut().character.combat.health;
                    hp.hit_points = (native_health * HEALTH_SCALE).min(hp.max_hit_points);
                    world.avatar_mut().character.combat.dead = native_health <= 0.;
                }
                Ok(world)
            })) {
                Ok(world) => {
                    if let Err(error) = publish_wizard(&world, state.classic, &mut sim.game) {
                        state.notices = format!("Wizard state could not be saved: {error}");
                    }
                    println!(
                        "MAGICKA: hosted wizard and Postal collision for {}",
                        state.map
                    );
                    let health = (world.avatar().character.combat.health.hit_points / HEALTH_SCALE)
                        .ceil() as i32;
                    sim.game
                        .world
                        .vm
                        .set_prop(pawn, "Health", Value::Int(health));
                    state.published_health = Some(health);
                    state.pawn = Some(pawn);
                    state.death_delivered = world.avatar().dead();
                    state.world = Some(world);
                }
                Err(error) => {
                    eprintln!("MAGICKA: {error}");
                    sim.game.save_extension_errors.insert("magicka".into(), error.clone());
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
    if !ui.game_look || ui.captured || sim.paused { return; }
    if keys.just_pressed(KeyCode::F6) {
        state.classic = !state.classic;
    }
    let Some(wizard) = state.world.as_mut() else {
        return;
    };
    let vm = &mut sim.game.world.vm;
    p2game::damage_bridge::register(pawn);
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
    let orb = state.fp_orb.filter(|_| !state.classic);
    let aim_dir = (aim - orb.unwrap_or(Vec3::from(position))).normalize_or_zero();
    wizard.place(position, aim_dir.to_array());
    let body_index = wizard.avatar().body;
    // Match capsule centres: both engines use centre positions, with their
    // respective radius/length determining the feet offset.
    let postal_half = vm
        .get_prop(pawn, "CollisionHeight")
        .map(Value::as_float)
        .unwrap_or(40.);
    let mage_half = -wizard.body().height_offset() * p2magicka::UNREAL_UNITS_PER_METRE;
    wizard.session.world.physics.bodies[body_index].position[1] +=
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
                match wizard.attach_pawn(pos, hp * HEALTH_SCALE, radius, height) {
                    Ok(handle) => {
                        state.mirrors.insert(
                            actor,
                            Mirror {
                                handle,
                                health: hp,
                                remainder: 0.,
                                burning: false,
                                frozen_speed: None,
                                gibbed: false,
                            },
                        );
                    }
                    Err(error) => {
                        eprintln!("MAGICKA pawn: {error}");
                        continue;
                    }
                }
            }
            if let Ok(words) = std::env::var("MAGICKA_DUMP_FN") {
                if state.mirrors.len() == 1 {
                    let mut c = Some(vm.objects[actor].class);
                    while let Some(ci) = c {
                        let f = vm.classes[ci].field;
                        let sp = vm.pkgs[f.pkg as usize].clone();
                        let names: Vec<String> = sp
                            .children(f.export as usize)
                            .into_iter()
                            .filter(|&k| matches!(sp.objects[k], Some(uscript::ScriptObject::Function(_))))
                            .map(|k| sp.export_name(k).to_string())
                            .filter(|n| words.split(',').any(|w| n.to_ascii_lowercase().contains(w)))
                            .collect();
                        println!("MAGICKA FN {}: {}", vm.classes[ci].name, names.join(" "));
                        if vm.classes[ci].super_.is_none() {
                            let damage = vm.find_class("Engine", "DamageType").ok();
                            let all: Vec<String> = (0..vm.classes.len())
                                .filter(|&k| damage.is_some_and(|d| vm.is_child_of(k, d)))
                                .map(|k| format!("{}.{}", vm.classes[k].package, vm.classes[k].name))
                                .collect();
                            println!("MAGICKA DAMAGETYPES {}", all.join(" "));
                        }
                        c = vm.classes[ci].super_;
                    }
                }
            }
            let mirror = &mut state.mirrors.get_mut(&actor).unwrap();
            if let Some(npc) = wizard.session.world.npcs.get_mut(&mirror.handle) {
                npc.character.combat.health.hit_points += (hp - mirror.health) * HEALTH_SCALE;
                let body = &mut wizard.session.world.physics.bodies[npc.body];
                body.move_to(p2magicka::to_magicka(pos), body.orientation);
                body.velocity = [0.; 3];
            }
            mirror.health = hp;
        }
    }
    // The existing VM TakeDamage watch consumes the original hit before
    // Postal can kill the player; Magicka owns shields, robes and statuses.
    let incoming = p2game::damage_bridge::take(pawn);
    let last_hit = incoming.last().cloned();
    for hit in incoming {
        let element = postal_damage_element(vm, hit.damage_type);
        let attacker = hit.instigator.and_then(|a| {
            if a == pawn {
                Some(wizard.avatar().handle)
            } else {
                state.mirrors.get(&a).map(|m| m.handle)
            }
        });
        wizard.receive_damage(
            p2magicka::magicka_sim::damage::Damage {
                attack: p2magicka::magicka_sim::damage::attack::DAMAGE
                    | if element == p2magicka::magicka_sim::spells::Elements::EARTH {
                        0
                    } else {
                        p2magicka::magicka_sim::damage::attack::STATUS
                    },
                element: element.0,
                amount: hit.amount * HEALTH_SCALE,
                magnitude: 1.,
            },
            attacker,
            p2magicka::to_magicka(hit.location),
        );
        println!(
            "MAGICKA INCOMING amount={} element={:?}",
            hit.amount, element
        );
    }
    // Native food/medkits and direct health changes remain useful to a wizard.
    let native_health = vm.get_prop(pawn, "Health").map(Value::as_int).unwrap_or(0);
    if let Some(previous) = state.published_health {
        let difference = native_health - previous;
        if difference != 0 {
            let element = if difference > 0 {
                p2magicka::magicka_sim::spells::Elements::LIFE
            } else {
                p2magicka::magicka_sim::spells::Elements::EARTH
            };
            wizard.receive_damage(
                p2magicka::magicka_sim::damage::Damage {
                    attack: p2magicka::magicka_sim::damage::attack::DAMAGE,
                    element: element.0,
                    amount: -(difference as f32) * HEALTH_SCALE,
                    magnitude: 1.,
                },
                None,
                wizard.body().position,
            );
        }
    }
    let dt = time.delta_secs().clamp(0., 0.05);
    state.wardrobe.update(wizard, &keys);
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
    let mut input = PlayerInput {
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
    if state.wardrobe.open {
        input = PlayerInput {
            pad_movement: Some([0., 0.]),
            ..default()
        };
    }
    wizard.input(input, aim.to_array(), dt);
    if let Some(orb) = orb {
        // Casts, sprays and beams leave the staff seen in view, toward the crosshair.
        wizard.set_staff_orb(orb.to_array(), aim_dir.to_array());
    }
    state.clock += dt;
    if let Some((_, spell)) = wizard.avatar().casting_spell() {
        state.recent_elements = (spell.element.0, state.clock);
    }
    // Pushes, levitation and knockback of the hosted characters this step.
    let velocities_before: HashMap<u16, [f32; 3]> = state
        .mirrors
        .values()
        .filter_map(|m| {
            let npc = wizard.session.world.npcs.get(&m.handle)?;
            Some((m.handle, wizard.session.world.physics.bodies[npc.body].position))
        })
        .collect();
    match wizard.step(dt) {
        Ok(events) => {
            for event in events {
                if let AvatarEvent::SpellCast {
                    spell, cast_type, ..
                } = event
                {
                    state.notices = format!("{:?}: {:?}", cast_type, spell.element);
                    state.recent_elements = (spell.element.0, state.clock);
                    println!("MAGICKA CAST {:?} {:?}", cast_type, spell);
                }
            }
        }
        Err(error) => {
            state.error = Some(error);
            return;
        }
    }
    if std::env::var_os("MAGICKA_TRACE").is_some() {
        println!(
            "MAGICKA TRACE native={native_health} published={:?} hp={} pos={:?} body={:?}",
            state.published_health,
            wizard.avatar().character.combat.health.hit_points,
            position,
            wizard.body().position
        );
    }
    let health = (wizard.avatar().character.combat.health.hit_points / HEALTH_SCALE)
        .ceil()
        .max(0.) as i32;
    vm.set_prop(pawn, "Health", Value::Int(health));
    state.published_health = Some(health);
    if wizard.avatar().dead() && !state.death_delivered {
        state.death_delivered = true;
        let killer = last_hit
            .as_ref()
            .and_then(|h| h.instigator)
            .and_then(|a| vm.get_prop(a, "Controller").and_then(Value::as_obj));
        let damage_type = last_hit
            .as_ref()
            .and_then(|h| h.damage_type)
            .or_else(|| vm.find_class("Engine", "DamageType").ok());
        vm.fire_event(
            pawn,
            "Died",
            vec![
                Value::Object(killer),
                Value::Class(damage_type),
                Value::Vector(last_hit.as_ref().map(|h| h.location).unwrap_or(position)),
            ],
        );
        println!("MAGICKA: wizard death delivered to Postal pawn");
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
    let melee = matches!(
        wizard.avatar().motion.state,
        p2magicka::magicka_runtime::character_motion::CharacterState::Attack
    );
    let spell_elements = if state.clock - state.recent_elements.1 < 1.5 {
        state.recent_elements.0
    } else {
        0
    };
    for (&actor, mirror) in &mut state.mirrors {
        if vm.objects.get(actor).is_none_or(|o| o.deleted) {
            continue;
        }
        let npc = wizard.session.world.npcs.get(&mirror.handle);
        // A character the rewrite removed on death (overkill, disintegration)
        // takes its remaining health in one hit.
        let new_health = npc.map_or(-1000., |n| n.character.combat.health.hit_points / HEALTH_SCALE);
        let statuses = npc.map(|n| n.character.combat.statuses.current_mask).unwrap_or(0);
        let overkilled = npc.is_none_or(|n| n.character.combat.overkilled);
        let displacement = npc.and_then(|n| {
            let now = wizard.session.world.physics.bodies[n.body].position;
            let before = velocities_before.get(&mirror.handle)?;
            Some(Vec3::from(now) - Vec3::from(*before))
        });
        let damage = mirror.health - new_health + mirror.remainder;
        if std::env::var_os("MAGICKA_TRACE").is_some() && (statuses != 0 || damage.abs() > 0.01) {
            println!(
                "MAGICKA MIRROR {} postal={} magicka={new_health} statuses={statuses:#x} present={}",
                vm.object_name(actor),
                mirror.health,
                npc.is_some()
            );
        }
        let alive = vm.get_prop(actor, "Health").map(Value::as_float).unwrap_or(0.) > 0.;
        if damage >= 1. {
            let amount = damage.floor() as i32;
            let elements = if spell_elements != 0 { spell_elements } else { status_elements(statuses) };
            let class = postal_damage_class(vm, elements, melee, amount);
            // Magicka pushes become Postal momentum (kg·uu/s on a ~100 kg pawn).
            let momentum = displacement
                .map(|d| Vec3::from(p2magicka::to_postal(d.to_array())) / dt.max(1e-3) * 100.)
                .unwrap_or(Vec3::ZERO);
            vm.fire_event(
                actor,
                "TakeDamage",
                vec![
                    Value::Int(amount),
                    Value::Object(Some(pawn)),
                    Value::Vector(vm.actor_loc(actor)),
                    Value::Vector(momentum.to_array()),
                    Value::Class(class),
                ],
            );
            if std::env::var_os("MAGICKA_TRACE").is_some() {
                println!(
                    "MAGICKA HIT {} {amount} {} statuses={statuses:#x} momentum={momentum:?}",
                    vm.object_name(actor),
                    class.map(|c| vm.classes[c].name.as_str()).unwrap_or("?")
                );
            }
            mirror.remainder = damage - amount as f32;
        } else if damage <= -1. {
            vm.set_prop(actor, "Health", Value::Int(new_health as i32));
            mirror.remainder = 0.;
        } else {
            mirror.remainder = damage;
        }
        let health_now = vm.get_prop(actor, "Health").map(Value::as_float).unwrap_or(0.);
        // Overkill (frozen shatter, huge blasts) gibs the Postal body too.
        if overkilled && !mirror.gibbed && (alive || damage >= 1.) && health_now <= 0. {
            mirror.gibbed = true;
            vm.fire_event(actor, "ChunkUp", vec![Value::Int(damage.max(100.) as i32)]);
            println!("MAGICKA overkill gibbed {}", vm.object_name(actor));
        }
        if health_now > 0. {
            use p2magicka::magicka_sim::status::Status as S;
            let has = |s: S| statuses & s as u16 != 0;
            if has(S::Burning) && !mirror.burning {
                mirror.burning = true;
                vm.fire_event(actor, "SetOnFire", vec![Value::Object(Some(pawn)), Value::Bool(false)]);
            } else if !has(S::Burning) && mirror.burning {
                mirror.burning = false;
                if has(S::Wet) {
                    let extinguish = vm.find_class("BaseFX", "ExtinguishDamage").ok();
                    vm.fire_event(
                        actor,
                        "TakeDamage",
                        vec![
                            Value::Int(0),
                            Value::Object(Some(pawn)),
                            Value::Vector(vm.actor_loc(actor)),
                            Value::Vector([0.; 3]),
                            Value::Class(extinguish),
                        ],
                    );
                }
            }
            // Frozen solid: no walking until the ice thaws or shatters.
            if has(S::Frozen) && mirror.frozen_speed.is_none() {
                mirror.frozen_speed = vm.get_prop(actor, "GroundSpeed").map(Value::as_float);
                vm.set_prop(actor, "GroundSpeed", Value::Float(0.));
                vm.set_prop(actor, "Velocity", Value::Vector([0.; 3]));
            } else if !has(S::Frozen) {
                if let Some(speed) = mirror.frozen_speed.take() {
                    vm.set_prop(actor, "GroundSpeed", Value::Float(speed));
                }
            }
        }
        mirror.health = health_now;
        // The native TakeDamage may apply armour or script immunity. Keep
        // subsequent damage deltas relative to that actual result.
        if let Some(npc) = wizard.session.world.npcs.get_mut(&mirror.handle) {
            npc.character.combat.health.hit_points = mirror.health * HEALTH_SCALE;
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
            "{}  HP {:.0}/{:.0}  {}  · F7 loadout\n{}\n{}\n{}\n{}",
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
    if let Err(error) = publish_wizard(wizard, state.classic, &mut sim.game) {
        state.notices = format!("Wizard state could not be saved: {error}");
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
struct FirstPersonHands {
    state: Option<p2magicka::magicka_runtime::character_motion::CharacterState>,
    since: f32,
    walk: f32,
}
#[derive(Default)]
struct WizardDraws {
    map: String,
    loadout: Option<Loadout>,
    colour: [f32; 3],
    parts: Vec<(p2magicka::visual::Part, Entity, Handle<Mesh>)>,
}
fn visuals(
    mut commands: Commands,
    mut state: NonSendMut<MagicState>,
    sim: NonSend<Sim>,
    ui: Res<UiInput>,
    mut draws: Local<WizardDraws>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut visibility: Query<&mut Visibility>,
    time: Res<Time>,
    mut fp: Local<FirstPersonHands>,
) {
    let state = &mut *state;
    let Some(wizard) = &state.world else { return };
    let loadout = wizard.equipped_loadout();
    if draws.map != state.map || draws.loadout.as_ref() != Some(&loadout) || draws.colour != wizard.robe_colour {
        for (_, entity, _) in draws.parts.drain(..) {
            commands.entity(entity).despawn();
        }
        draws.map.clone_from(&state.map);
        draws.loadout = Some(loadout);
        draws.colour = wizard.robe_colour;
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
                        alpha_mode: if part.cutout { AlphaMode::Mask(0.5) } else { AlphaMode::Opaque },
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
    // First person: the staff (right hand) and melee weapon (left hand) are
    // held in view instead of following the third-person animation, with the
    // cast thrust, charge tremble and melee swing driven by the avatar state.
    let motion_state = wizard.avatar().motion.state;
    if Some(motion_state) != fp.state {
        fp.state = Some(motion_state);
        fp.since = time.elapsed_secs();
    }
    let t = time.elapsed_secs();
    let phase = t - fp.since;
    let walking = sim
        .game
        .player_controller
        .and_then(|pc| sim.game.world.vm.get_prop(pc, "Pawn")?.as_obj())
        .and_then(|pawn| sim.game.world.vm.get_prop(pawn, "Velocity"))
        .map(|v| Vec3::from(v.as_vec()).truncate().length() > 20.)
        .unwrap_or(false);
    fp.walk = (fp.walk + if walking { 1. } else { -1. } * time.delta_secs() * 4.).clamp(0., 1.);
    let held = eye_and_forward.map(|(eye, forward)| {
        use p2magicka::magicka_runtime::character_motion::CharacterState as S;
        let right = forward.cross(Vec3::Y).normalize_or(Vec3::X);
        let up = right.cross(forward);
        let bob = up * (t * 9.).sin() * 0.012 * fp.walk + right * (t * 4.5).cos() * 0.01 * fp.walk;
        let ease = |d: f32| (phase / d).clamp(0., 1.);
        let (cast, charge, attack, boost) = (
            matches!(motion_state, S::Cast),
            matches!(motion_state, S::Charge),
            matches!(motion_state, S::Attack),
            matches!(motion_state, S::Boost),
        );
        move |slot: i32| -> (Vec3, Vec3, Vec3, Vec3) {
            let (mut grip, mut axis, scale) = if slot == 1 {
                (
                    eye + forward * 0.62 + right * 0.3 - up * 0.42,
                    (up * 0.85 + forward * 0.4 - right * 0.12).normalize(),
                    0.42,
                )
            } else {
                (
                    eye + forward * 0.5 - right * 0.34 - up * 0.42,
                    (up * 0.55 + forward * 0.75 + right * 0.2).normalize(),
                    0.55,
                )
            };
            grip += bob;
            if slot == 1 && (cast || charge || boost) {
                let k = ease(0.12);
                grip += forward * 0.14 * k - right * 0.06 * k + up * 0.05 * k;
                axis = axis.lerp((forward * 0.75 + up * 0.65).normalize(), k).normalize();
                if charge {
                    grip += (right * (t * 47.).sin() + up * (t * 39.).cos()) * 0.006;
                }
                if boost {
                    grip += up * 0.08 * k;
                }
            }
            if slot == 0 && attack {
                // A right-to-left slash: the blade sweeps across the view.
                let k = ease(0.35);
                let angle = 1.1 - 2.6 * k;
                let swing = Quat::from_axis_angle(forward, angle)
                    * Quat::from_axis_angle(up, -0.6 * k);
                axis = (swing * (up * 0.3 + forward * 0.9)).normalize();
                grip += right * 0.3 * (k * std::f32::consts::PI).sin() + forward * 0.08;
            }
            let z = (forward - axis * forward.dot(axis)).normalize_or(-right);
            let x = axis.cross(z);
            (grip, x * scale, axis * scale, z * scale)
        }
    });
    // The staff orb sits at the top of the staff model along its held axis.
    state.fp_orb = held.as_ref().filter(|_| !state.classic).and_then(|held| {
        let staff = draws.parts.iter().find(|(p, ..)| p.slot == Some(1))?;
        let top = staff.0.geometry.positions.iter().map(|p| p[1]).fold(f32::MIN, f32::max);
        let (origin, _, y, _) = held(1);
        let tip = origin + y * top;
        Some(Vec3::new(tip.x, tip.z, tip.y) * p2view::scene::UU_PER_M)
    });
    for (part, entity, handle) in &draws.parts {
        if let Ok(mut v) = visibility.get_mut(*entity) {
            *v = if ui.player_view && (state.classic || part.slot.is_some()) {
                Visibility::Visible
            } else {
                Visibility::Hidden
            };
        }
        let positions = match (state.classic, part.slot, &held) {
            (false, Some(slot), Some(held)) => {
                let (origin, x, y, z) = held(slot);
                part.geometry
                    .positions
                    .iter()
                    .map(|p| (origin + x * p[0] + y * p[1] + z * p[2]).to_array())
                    .collect()
            }
            _ => p2magicka::visual::positions(part, &skin, &items),
        };
        if let Some(mut mesh) = meshes.get_mut(handle) {
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
            mesh.compute_smooth_normals();
        }
    }
}

/// Elements implied by a victim's statuses when no wizard spell is active.
fn status_elements(mask: u16) -> u32 {
    use p2magicka::magicka_sim::{spells::Elements as E, status::Status as S};
    let mut e = 0;
    for (status, element) in [
        (S::Burning, E::FIRE),
        (S::Wet, E::WATER),
        (S::Frozen, E::COLD),
        (S::Cold, E::COLD),
        (S::Poisoned, E::POISON),
        (S::Steamed, E::STEAM),
    ] {
        if mask & status as u16 != 0 {
            e |= element.0;
        }
    }
    e
}

/// The Postal damage class whose native reactions match a Magicka hit:
/// burning, shocked dancing, anthrax coughing, exploding into gibs.
fn postal_damage_class(
    vm: &mut uscript::vm::Vm,
    elements: u32,
    melee: bool,
    amount: i32,
) -> Option<uscript::vm::ClassId> {
    use p2magicka::magicka_sim::spells::Elements as E;
    let has = |e: E| elements & e.0 != 0;
    let name = if melee && elements == 0 {
        "CuttingDamage"
    } else if has(E::LIGHTNING) {
        "ElectricalDamage"
    } else if has(E::STEAM) {
        // Steam scalds and soaks (Magicka's Wet status) rather than ignites;
        // BurnedDamage would set the Postal victim alight.
        "BludgeonDamage"
    } else if has(E::FIRE) {
        "BurnedDamage"
    } else if has(E::POISON) {
        "AnthDamage"
    } else if has(E::ARCANE) && amount >= 40 {
        "ExplodedDamage"
    } else if has(E::ARCANE) {
        "BloodMakingDamage"
    } else if has(E::ICE) {
        "CuttingDamage"
    } else if has(E::EARTH) {
        "SmashDamage"
    } else if has(E::WATER) || has(E::COLD) {
        "BludgeonDamage"
    } else if melee {
        "CuttingDamage"
    } else {
        "BodyDamage"
    };
    match vm.find_class("BaseFX", name) {
        Ok(class) => Some(class),
        Err(_) => vm.find_class("Engine", "DamageType").ok(),
    }
}

/// Postal damage classes retain their names/ancestry at runtime. Most attacks
/// are physical; elemental classes are translated into the matching Magicka
/// damage channel so the original robe/equipment tables and wards apply.
fn postal_damage_element(
    vm: &uscript::vm::Vm,
    damage_type: Option<uscript::vm::ClassId>,
) -> p2magicka::magicka_sim::spells::Elements {
    use p2magicka::magicka_sim::spells::Elements as E;
    let mut class = damage_type;
    while let Some(id) = class {
        let Some(info) = vm.classes.get(id) else {
            break;
        };
        let name = info.name.to_ascii_lowercase();
        if name.contains("fire") || name.contains("burn") || name.contains("napalm") {
            return E::FIRE;
        }
        if name.contains("electric") || name.contains("shock") || name.contains("lightning") {
            return E::LIGHTNING;
        }
        if name.contains("poison") || name.contains("anthrax") || name.contains("chemical") {
            return E::POISON;
        }
        if name.starts_with("ice") || name.contains("freeze") || name.contains("frost") {
            return E::COLD;
        }
        class = info.super_;
    }
    E::EARTH
}
