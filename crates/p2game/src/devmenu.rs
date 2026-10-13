//! Developer menu (F10) and 3D editor mode (F11).
//!
//! A tabbed panel drawn with Bevy UI above the game:
//! - **Cheats**: god mode, ghost/fly, all weapons and ammo, health, kill everyone, ghost town,
//!   the original P2CheatManager codes (they run through the script exactly as typed in the
//!   console; P2CheatManager stubs out the engine God/Fly/Ghost/SloMo, so those are done here).
//! - **World**: game speed, gravity, pause, spawn NPCs and animals, explosions at the crosshair,
//!   teleport to the crosshair.
//! - **Player**: speed, jump height, size of the hud numbers, field of view.
//! - **View**: render debug modes (wireframe, fullbright, no fog, no textures) and overlays drawn
//!   with gizmos (actor bounds, labels, collision cylinders, navigation network, pawn sight lines,
//!   triggers/volumes, world axes and grid), plus the F3 stats overlay.
//! - **Editor**: a free-camera editor. The world is paused, the mouse is free, clicking picks the
//!   actor under the cursor (the selection is outlined with a transform gizmo), the inspector
//!   lists its properties, and keys move/rotate/duplicate/delete it or drop it to the floor.
//!
//! Everything here changes the running script world the way console commands do; it does not
//! write files. Keys: F10 menu, F11 editor, Esc closes.

use crate::window::{FlyCam, Sim};
use uscript::host::Host as _;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::pbr::wireframe::{WireframeConfig, WireframePlugin};
use bevy::prelude::*;
use p2view::scene::{UU_PER_M, to_bevy};
use uscript::vm::{ObjId, Value, Vm};

pub(crate) struct DevMenuPlugin;

impl Plugin for DevMenuPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<WireframePlugin>() {
            app.add_plugins(WireframePlugin::default());
        }
        app.insert_resource(WireframeConfig { global: false, default_color: Color::srgb(0.3, 1.0, 0.6), ..default() })
            .init_resource::<DevState>()
            .add_systems(Startup, spawn_panel)
            .add_systems(
                Update,
                (dev_input, apply_actions, editor_camera, apply_render_modes, draw_overlays, refresh_panel, draw_labels)
                    .chain()
                    .after(crate::ui::ui_frame)
                    .before(crate::ui::follow_player),
            )
            .add_systems(Update, editor_block_input.before(crate::ui::ui_frame))
            // fp_weapon re-activates its camera every frame; hide it afterwards.
            .add_systems(Update, hide_fp_weapon.after(crate::ui::ui_frame).after(apply_render_modes))
            .add_systems(PostUpdate, hide_fp_weapon);
    }
}

// ---------------------------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) enum Tab {
    #[default]
    Cheats,
    World,
    Player,
    View,
    Editor,
}

impl Tab {
    const ALL: [Tab; 5] = [Tab::Cheats, Tab::World, Tab::Player, Tab::View, Tab::Editor];
    fn label(self) -> &'static str {
        match self {
            Tab::Cheats => "CHEATS",
            Tab::World => "WORLD",
            Tab::Player => "PLAYER",
            Tab::View => "VIEW",
            Tab::Editor => "EDITOR",
        }
    }
}

/// Render debug mode (one at a time).
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) enum RenderMode {
    #[default]
    Lit,
    Wireframe,
    WireOverlay,
    Fullbright,
    Untextured,
}

impl RenderMode {
    const ALL: [RenderMode; 5] = [RenderMode::Lit, RenderMode::Wireframe, RenderMode::WireOverlay, RenderMode::Fullbright, RenderMode::Untextured];
    fn label(self) -> &'static str {
        match self {
            RenderMode::Lit => "Lit (game)",
            RenderMode::Wireframe => "Wireframe",
            RenderMode::WireOverlay => "Lit + wire",
            RenderMode::Fullbright => "Fullbright",
            RenderMode::Untextured => "Clay (no textures)",
        }
    }
}

#[derive(Default, Clone, Copy)]
pub(crate) struct Overlays {
    bounds: bool,
    labels: bool,
    collision: bool,
    nav: bool,
    sight: bool,
    volumes: bool,
    axes: bool,
    no_fog: bool,
    hide_hud_weapon: bool,
}

#[derive(Resource, Default)]
pub(crate) struct DevState {
    pub(crate) open: bool,
    pub(crate) editor: bool,
    tab: Tab,
    render: RenderMode,
    applied_render: RenderMode,
    ov: Overlays,
    /// Toggles kept host-side (script has no state for them).
    god: bool,
    infinite_ammo: bool,
    freeze_ai: bool,
    game_speed: f32,
    gravity_scale: f32,
    speed_scale: f32,
    jump_scale: f32,
    /// Queued actions from buttons/keys, run with the VM.
    pending: Vec<Action>,
    /// Status line ("God mode on", "Spawned Police").
    toast: Option<(String, f32)>,
    /// Editor selection and tools.
    selected: Option<ObjId>,
    gizmo_mode: GizmoMode,
    snap: bool,
    /// The fly camera's pose when the editor opened (restored when leaving if the player view
    /// isn't driving the camera).
    level_serial: u64,
    /// Seconds since the panel text was rebuilt.
    since_refresh: f32,
    rebuild: bool,
    /// Pause state of the script world before the editor took over.
    was_paused: bool,
    /// Editor: time runs (Space toggles). Off = the world is frozen, the editor default.
    editor_time: bool,
    /// Last property list shown in the inspector (to avoid rebuilding text every frame).
    inspector: String,
    stats: String,
    /// Editor: the panel is collapsed to its top-right toggle (H or the button).
    panel_hidden: bool,
    /// Editor: a gizmo handle being dragged with the left mouse button.
    drag: Option<Drag>,
}

/// A gizmo drag in progress. Move: along one axis (Bevy space, 0=X 1=Y(up) 2=Z); rotate: yaw from
/// horizontal mouse travel.
#[derive(Clone, Copy, Debug)]
struct Drag {
    axis: Option<usize>,
    start_loc: Vec3,
    start_t: f32,
    start_yaw: i32,
    start_cursor: Vec2,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum GizmoMode {
    #[default]
    Move,
    Rotate,
}

#[derive(Clone, Debug)]
enum Action {
    Exec(&'static str),
    God,
    Ghost,
    Fly,
    Walk,
    FullHealth,
    InfiniteAmmo,
    KillPawns,
    FreezeAi,
    GameSpeed(f32),
    Gravity(f32),
    Speed(f32),
    Jump(f32),
    Summon(&'static str),
    Explode,
    TeleportCrosshair,
    Launch,
    PauseWorld,
    /// Editor: freeze/unfreeze time (Space).
    ToggleEditorTime,
    // editor
    Pick(Vec3, Vec3),
    Select(Option<ObjId>),
    Nudge([f32; 3]),
    /// Gizmo drag: absolute location (UU) / yaw.
    SetLocation([f32; 3]),
    SetYaw(i32),
    Rotate(i32),
    Delete,
    Duplicate,
    DropToFloor,
    TeleportToSelected,
    BringSelected,
    Possess,
    Kill,
}

impl DevState {
    fn toast(&mut self, s: impl Into<String>) {
        let s = s.into();
        if std::env::var_os("P2GAME_DEV_TRACE").is_some() {
            println!("devmenu toast: {s}");
        }
        self.toast = Some((s, 3.0));
        self.rebuild = true;
    }
}

// ---------------------------------------------------------------------------------------------
// Panel UI
// ---------------------------------------------------------------------------------------------

const BG: Color = Color::srgba(0.055, 0.06, 0.07, 0.94);
const BG2: Color = Color::srgba(0.10, 0.11, 0.13, 0.96);
const ACCENT: Color = Color::srgb(0.93, 0.26, 0.21);
const TEXT: Color = Color::srgb(0.88, 0.90, 0.92);
const DIM: Color = Color::srgb(0.55, 0.58, 0.62);
const ON: Color = Color::srgb(0.30, 0.85, 0.45);
const BTN: Color = Color::srgb(0.16, 0.17, 0.20);
const BTN_HOVER: Color = Color::srgb(0.24, 0.26, 0.30);

#[derive(Component)]
struct DevRoot;
#[derive(Component)]
struct DevBody;
#[derive(Component)]
struct DevTabButton(Tab);
#[derive(Component)]
struct DevButton(ButtonKind);
#[derive(Component)]
struct DevToast;
#[derive(Component)]
struct EditorBar;
#[derive(Component)]
struct EditorBarText;
#[derive(Component)]
struct ActorLabel;

/// What a panel button does. Toggles show their state; actions are queued.
#[derive(Clone, Debug)]
enum ButtonKind {
    Act(Action),
    Render(RenderMode),
    Overlay(fn(&mut Overlays) -> &mut bool),
    StatsOverlay,
    Editor,
    Gizmo(GizmoMode),
    Snap,
    TogglePanel,
}

fn spawn_panel(mut commands: Commands) {
    // A camera only for Bevy UI (this panel, labels, the F3 overlay), above the canvas HUD and the
    // first-person weapon. Bevy otherwise puts UI on the highest-order camera, which is the weapon
    // overlay, and that one is switched off when no weapon is drawn. Its render layer has no
    // world content.
    commands.spawn((
        Camera2d,
        Camera { order: 200, clear_color: ClearColorConfig::None, ..default() },
        bevy::camera::visibility::RenderLayers::layer(30),
        IsDefaultUiCamera,
    ));
    commands.spawn((
        DevRoot,
        Node {
            position_type: PositionType::Absolute,
            right: px(12.0),
            top: px(12.0),
            width: px(430.0),
            max_height: Val::Percent(92.0),
            flex_direction: FlexDirection::Column,
            border: UiRect::all(px(1.0)),
            border_radius: BorderRadius::all(px(6.0)),
            overflow: Overflow::clip(),
            ..default()
        },
        BackgroundColor(BG),
        BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.08)),
        GlobalZIndex(i32::MAX - 2),
        Visibility::Hidden,
    ));
    // Editor status bar along the bottom of the screen.
    commands
        .spawn((
            EditorBar,
            Node {
                position_type: PositionType::Absolute,
                left: px(0.0),
                right: px(0.0),
                bottom: px(0.0),
                padding: UiRect::axes(px(12.0), px(6.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.72)),
            GlobalZIndex(i32::MAX - 3),
            Visibility::Hidden,
        ))
        .with_children(|p| {
            p.spawn((EditorBarText, Text::new(""), TextFont { font_size: bevy::text::FontSize::Px(13.0), ..default() }, TextColor(TEXT)));
        });
}

fn text(p: &mut ChildSpawnerCommands, s: impl Into<String>, size: f32, color: Color) {
    p.spawn((Text::new(s.into()), TextFont { font_size: bevy::text::FontSize::Px(size), ..default() }, TextColor(color)));
}

fn section(p: &mut ChildSpawnerCommands, title: &str) {
    p.spawn(Node { margin: UiRect { top: px(10.0), bottom: px(4.0), ..default() }, ..default() }).with_children(|p| text(p, title, 11.0, DIM));
}

fn button(p: &mut ChildSpawnerCommands, label: &str, kind: ButtonKind, on: Option<bool>) {
    let (bg, fg) = match on {
        Some(true) => (Color::srgb(0.13, 0.30, 0.18), ON),
        _ => (BTN, TEXT),
    };
    p.spawn((
        Button,
        DevButton(kind),
        Node {
            padding: UiRect { left: px(if on.is_some() { 8.0 } else { 10.0 }), right: px(10.0), top: px(5.0), bottom: px(5.0) },
            margin: UiRect::all(px(2.0)),
            border: UiRect { left: px(if on.is_some() { 3.0 } else { 1.0 }), right: px(1.0), top: px(1.0), bottom: px(1.0) },
            border_radius: BorderRadius::all(px(4.0)),
            ..default()
        },
        BackgroundColor(bg),
        BorderColor {
            left: match on {
                Some(true) => ON,
                Some(false) => Color::srgba(1.0, 1.0, 1.0, 0.18),
                None => Color::srgba(1.0, 1.0, 1.0, 0.06),
            },
            ..BorderColor::all(if on == Some(true) { ON.with_alpha(0.5) } else { Color::srgba(1.0, 1.0, 1.0, 0.06) })
        },
    ))
    .with_children(|p| {
        text(p, label, 13.0, fg);
    });
}

fn row(p: &mut ChildSpawnerCommands, f: impl FnOnce(&mut ChildSpawnerCommands)) {
    p.spawn(Node { flex_direction: FlexDirection::Row, flex_wrap: FlexWrap::Wrap, ..default() }).with_children(f);
}

fn build_panel(commands: &mut Commands, root: Entity, st: &DevState, overlay_on: bool) {
    commands.entity(root).despawn_children();
    if st.editor && st.panel_hidden {
        // Collapsed: only a small "show panel" tab in the top-right corner.
        commands.entity(root).with_children(|p| {
            row(p, |p| button(p, "\u{25C0} Dev panel (H)", ButtonKind::TogglePanel, None));
        });
        return;
    }
    commands.entity(root).with_children(|p| {
        // Title bar.
        p.spawn((
            Node { flex_direction: FlexDirection::Row, justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::Center, padding: UiRect::axes(px(12.0), px(8.0)), border: UiRect::bottom(px(2.0)), ..default() },
            BackgroundColor(BG2),
            BorderColor::all(ACCENT),
        ))
        .with_children(|p| {
            p.spawn(Node { flex_direction: FlexDirection::Row, align_items: AlignItems::Center, column_gap: px(8.0), ..default() }).with_children(|p| {
                text(p, "POSTAL2RUST", 15.0, ACCENT);
                text(p, "DEVELOPER", 15.0, TEXT);
            });
            if st.editor {
                button(p, "Hide \u{25B6} (H)", ButtonKind::TogglePanel, None);
            } else {
                text(p, "F10 close  F11 editor", 11.0, DIM);
            }
        });
        // Tabs.
        p.spawn((Node { flex_direction: FlexDirection::Row, padding: UiRect::axes(px(6.0), px(4.0)), ..default() }, BackgroundColor(BG2))).with_children(|p| {
            for t in Tab::ALL {
                let sel = t == st.tab;
                p.spawn((
                    Button,
                    DevTabButton(t),
                    Node { padding: UiRect::axes(px(10.0), px(6.0)), margin: UiRect::horizontal(px(1.0)), border: UiRect::bottom(px(2.0)), ..default() },
                    BackgroundColor(if sel { BG } else { Color::NONE }),
                    BorderColor::all(if sel { ACCENT } else { Color::NONE }),
                ))
                .with_children(|p| text(p, t.label(), 12.0, if sel { TEXT } else { DIM }));
            }
        });
        // Body.
        p.spawn((DevBody, Node { flex_direction: FlexDirection::Column, padding: UiRect::axes(px(12.0), px(6.0)), overflow: Overflow::scroll_y(), ..default() })).with_children(|p| {
            body(p, st, overlay_on);
        });
        // Footer: toast + stats.
        p.spawn((Node { flex_direction: FlexDirection::Column, padding: UiRect::axes(px(12.0), px(6.0)), border: UiRect::top(px(1.0)), ..default() }, BackgroundColor(BG2), BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.06))))
            .with_children(|p| {
                let t = st.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
                p.spawn((DevToast, Text::new(t), TextFont { font_size: bevy::text::FontSize::Px(12.0), ..default() }, TextColor(ON)));
                text(p, st.stats.clone(), 11.0, DIM);
            });
    });
}

fn body(p: &mut ChildSpawnerCommands, st: &DevState, overlay_on: bool) {
    use Action as A;
    use ButtonKind as B;
    match st.tab {
        Tab::Cheats => {
            section(p, "INVINCIBILITY & MOVEMENT");
            row(p, |p| {
                button(p, "God mode", B::Act(A::God), Some(st.god));
                button(p, "Ghost (noclip)", B::Act(A::Ghost), None);
                button(p, "Fly", B::Act(A::Fly), None);
                button(p, "Walk", B::Act(A::Walk), None);
            });
            section(p, "ARSENAL");
            row(p, |p| {
                button(p, "All weapons", B::Act(A::Exec("PackNHeat")), None);
                button(p, "Max ammo", B::Act(A::Exec("PayLoad")), None);
                button(p, "Infinite ammo", B::Act(A::InfiniteAmmo), Some(st.infinite_ammo));
                button(p, "Everything", B::Act(A::Exec("IAmSoLame")), None);
                button(p, "Mighty Foot", B::Act(A::Exec("MightyFoot")), None);
                button(p, "Dual wield", B::Act(A::Exec("DoubleTheGun")), None);
                button(p, "Karma gun", B::Act(A::Exec("KarmaGun")), None);
                button(p, "Rapid fire", B::Act(A::Exec("TheQuick")), None);
                button(p, "Head shots", B::Act(A::Exec("HeadShots")), None);
                button(p, "Cat guns", B::Act(A::Exec("RockinCats")), None);
                button(p, "Bouncing cats", B::Act(A::Exec("BoppinCats")), None);
                button(p, "Scissor MG", B::Act(A::Exec("NowWeDance")), None);
            });
            section(p, "ITEMS");
            row(p, |p| {
                button(p, "Full health", B::Act(A::FullHealth), None);
                button(p, "Medkits", B::Act(A::Exec("Healthful")), None);
                button(p, "Body armor", B::Act(A::Exec("BlockMyAss")), None);
                button(p, "Cash", B::Act(A::Exec("JewsForJesus")), None);
                button(p, "Doughnuts", B::Act(A::Exec("PiggyTreats")), None);
                button(p, "Catnip", B::Act(A::Exec("IAmTheOne")), None);
                button(p, "Cats", B::Act(A::Exec("LotsAPussy")), None);
                button(p, "Radar", B::Act(A::Exec("SwimWithFishes")), None);
                button(p, "Cop clothes", B::Act(A::Exec("IAmTheLaw")), None);
                button(p, "Gimp suit", B::Act(A::Exec("SmackDatAss")), None);
                button(p, "Dog treats", B::Act(A::Exec("BoyAndHisDog")), None);
            });
            section(p, "MAYHEM");
            row(p, |p| {
                button(p, "Kill all NPCs", B::Act(A::KillPawns), None);
                button(p, "Ghost town", B::Act(A::Exec("GhostTown")), None);
                button(p, "No cops", B::Act(A::Exec("Anarchist")), None);
                button(p, "Gary Colemans", B::Act(A::Exec("Whatchutalkinbout")), None);
                button(p, "Fanatics", B::Act(A::Exec("Osama")), None);
                button(p, "Dudes", B::Act(A::Exec("DudeVSDude")), None);
                button(p, "Zombies", B::Act(A::Exec("Zombification")), None);
                button(p, "Everyone pukes", B::Act(A::Exec("PukeAll")), None);
                button(p, "Apocalypse", B::Act(A::Exec("ApocalypseNow")), None);
                button(p, "Cops hated", B::Act(A::Exec("CopKilla")), None);
                button(p, "Pet dog", B::Act(A::Exec("DogLover")), None);
                button(p, "Super jump", B::Act(A::Exec("SuperMario")), None);
                button(p, "Super speed", B::Act(A::Exec("SonicSpeed")), None);
                button(p, "Moon gravity", B::Act(A::Exec("MoonMan")), None);
            });
        }
        Tab::World => {
            section(p, "TIME");
            row(p, |p| {
                button(p, if st.was_paused || st.editor { "Resume" } else { "Pause world" }, B::Act(A::PauseWorld), None);
                for (l, v) in [("0.1x", 0.1), ("0.25x", 0.25), ("0.5x", 0.5), ("1x", 1.0), ("2x", 2.0), ("4x", 4.0)] {
                    button(p, l, B::Act(A::GameSpeed(v)), Some((st.game_speed - v).abs() < 0.001));
                }
            });
            section(p, "GRAVITY");
            row(p, |p| {
                for (l, v) in [("Zero", 0.0), ("Moon", 0.17), ("Half", 0.5), ("Normal", 1.0), ("Heavy", 2.5)] {
                    button(p, l, B::Act(A::Gravity(v)), Some((st.gravity_scale - v).abs() < 0.001));
                }
            });
            section(p, "AI");
            row(p, |p| {
                button(p, "Freeze AI", B::Act(A::FreezeAi), Some(st.freeze_ai));
            });
            section(p, "SPAWN AT CROSSHAIR");
            row(p, |p| {
                for (l, c) in [
                    ("Bystander", "BasePeople.Bystanders"),
                    ("Cop", "BasePeople.Police"),
                    ("Soldier", "BasePeople.Military"),
                    ("Fanatic", "BasePeople.Fanatics"),
                    ("Redneck", "BasePeople.Rednecks"),
                    ("Protestor", "BasePeople.Protestors"),
                    ("Zombie", "BasePeople.AWZombie"),
                    ("Gary", "People.Gary"),
                    ("Krotchy", "People.Krotchy"),
                    ("Dog", "People.DogPawn"),
                    ("Cat", "People.CatPawn"),
                    ("Cow", "People.CowPawn"),
                    ("Elephant", "People.ElephantPawn"),
                ] {
                    button(p, l, B::Act(A::Summon(c)), None);
                }
            });
            section(p, "TOOLS");
            row(p, |p| {
                button(p, "Explosion at crosshair", B::Act(A::Explode), None);
                button(p, "Teleport to crosshair", B::Act(A::TeleportCrosshair), None);
                button(p, "Launch me", B::Act(A::Launch), None);
            });
        }
        Tab::Player => {
            section(p, "RUN SPEED");
            row(p, |p| {
                for (l, v) in [("0.5x", 0.5), ("1x", 1.0), ("2x", 2.0), ("3x", 3.0), ("5x", 5.0)] {
                    button(p, l, B::Act(A::Speed(v)), Some((st.speed_scale - v).abs() < 0.001));
                }
            });
            section(p, "JUMP HEIGHT");
            row(p, |p| {
                for (l, v) in [("1x", 1.0), ("2x", 2.0), ("3x", 3.0), ("6x", 6.0)] {
                    button(p, l, B::Act(A::Jump(v)), Some((st.jump_scale - v).abs() < 0.001));
                }
            });
            section(p, "CONSOLE");
            row(p, |p| {
                button(p, "Behind view", B::Act(A::Exec("BehindView 1")), None);
                button(p, "First person", B::Act(A::Exec("BehindView 0")), None);
                button(p, "Suicide", B::Act(A::Exec("Suicide")), None);
                button(p, "Quick save", B::Act(A::Exec("QuickSave")), None);
                button(p, "Quick load", B::Act(A::Exec("QuickLoad")), None);
                button(p, "Screenshot", B::Act(A::Exec("shot")), None);
            });
        }
        Tab::View => {
            section(p, "RENDER MODE");
            row(p, |p| {
                for m in RenderMode::ALL {
                    button(p, m.label(), B::Render(m), Some(st.render == m));
                }
            });
            section(p, "OVERLAYS");
            row(p, |p| {
                button(p, "Stats (F3)", B::StatsOverlay, Some(overlay_on));
                button(p, "Actor bounds", B::Overlay(|o| &mut o.bounds), Some(st.ov.bounds));
                button(p, "Actor names", B::Overlay(|o| &mut o.labels), Some(st.ov.labels));
                button(p, "Collision", B::Overlay(|o| &mut o.collision), Some(st.ov.collision));
                button(p, "Nav network", B::Overlay(|o| &mut o.nav), Some(st.ov.nav));
                button(p, "AI sight", B::Overlay(|o| &mut o.sight), Some(st.ov.sight));
                button(p, "Triggers", B::Overlay(|o| &mut o.volumes), Some(st.ov.volumes));
                button(p, "Axes + grid", B::Overlay(|o| &mut o.axes), Some(st.ov.axes));
                button(p, "No fog", B::Overlay(|o| &mut o.no_fog), Some(st.ov.no_fog));
                button(p, "Hide weapon", B::Overlay(|o| &mut o.hide_hud_weapon), Some(st.ov.hide_hud_weapon));
            });
        }
        Tab::Editor => {
            row(p, |p| {
                button(p, if st.editor { "Exit editor (F11)" } else { "Enter editor (F11)" }, B::Editor, Some(st.editor));
                button(p, "Move (G)", B::Gizmo(GizmoMode::Move), Some(st.gizmo_mode == GizmoMode::Move));
                button(p, "Rotate (R)", B::Gizmo(GizmoMode::Rotate), Some(st.gizmo_mode == GizmoMode::Rotate));
                button(p, "Snap 16uu", B::Snap, Some(st.snap));
                button(p, if st.editor_time { "Freeze time (Space)" } else { "Run time (Space)" }, B::Act(A::ToggleEditorTime), Some(st.editor_time));
            });
            section(p, "SELECTION");
            row(p, |p| {
                button(p, "Drop to floor (End)", B::Act(A::DropToFloor), None);
                button(p, "Duplicate (Ctrl+D)", B::Act(A::Duplicate), None);
                button(p, "Delete (Del)", B::Act(A::Delete), None);
                button(p, "Kill", B::Act(A::Kill), None);
                button(p, "Go to (F)", B::Act(A::TeleportToSelected), None);
                button(p, "Bring here", B::Act(A::BringSelected), None);
                button(p, "Possess", B::Act(A::Possess), None);
                button(p, "Deselect", B::Act(A::Select(None)), None);
            });
            section(p, "INSPECTOR");
            text(p, if st.inspector.is_empty() { "Nothing selected. In the editor, left-click an actor.".to_string() } else { st.inspector.clone() }, 12.0, TEXT);
            section(p, "CONTROLS");
            text(
                p,
                "RMB drag look   WASD/QE fly   Shift fast\nLMB select, drag gizmo arrows/ring   G/R move/rotate mode   H hide panel\nArrows/PgUp/PgDn nudge (move) or turn (rotate)\nEnd drop   Ctrl+D duplicate   Del delete   F go to   Space freeze/unfreeze time",
                11.0,
                DIM,
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------------------------

/// While the editor or the menu (with the cursor over it) is up, the game gets no input: clear
/// the key/mouse state before the script front end reads it.
fn editor_block_input(st: Res<DevState>, mut keys: ResMut<ButtonInput<KeyCode>>, mut mouse: ResMut<ButtonInput<MouseButton>>, mut motion: ResMut<AccumulatedMouseMotion>, mut blocking: Local<bool>) {
    if !(st.editor || st.open) {
        *blocking = false;
    }
    if st.editor || st.open {
        // Keep our own keys readable in later systems via `just_pressed` copies taken in
        // dev_input (which runs after ui_frame); the game must not see clicks or look.
        let held: Vec<KeyCode> = keys.get_pressed().copied().collect();
        DEV_KEYS.with(|k| {
            let mut k = k.borrow_mut();
            k.just = keys.get_just_pressed().copied().collect();
            k.held = held.clone();
            k.mouse_just = mouse.get_just_pressed().copied().collect();
            k.mouse_held = mouse.get_pressed().copied().collect();
            k.motion = motion.delta;
        });
        // The game camera stays locked while the menu or editor is up: no mouse look, no keys
        // (playtest: moving the mouse with the panel open turned the Dude). On the first blocked
        // frame held keys are released (the game sees Release events and stops walking/firing);
        // after that each frame's presses/releases are dropped (the game only reads those).
        if !*blocking {
            *blocking = true;
            keys.clear();
            for k in held {
                keys.release(k);
            }
        } else {
            keys.clear();
        }
        motion.delta = Vec2::ZERO;
        mouse.clear();
    } else {
        DEV_KEYS.with(|k| {
            let mut k = k.borrow_mut();
            k.just = keys.get_just_pressed().copied().collect();
            k.held = keys.get_pressed().copied().collect();
            k.mouse_just.clear();
            k.mouse_held.clear();
            k.motion = Vec2::ZERO;
        });
    }
}

#[derive(Default)]
struct DevKeys {
    just: Vec<KeyCode>,
    held: Vec<KeyCode>,
    mouse_just: Vec<MouseButton>,
    mouse_held: Vec<MouseButton>,
    motion: Vec2,
}

/// This frame's keyboard/mouse as captured before the game consumed it. A process-wide static
/// (not thread-local): Bevy runs these systems on different worker threads.
struct DevKeysCell(std::sync::Mutex<Option<DevKeys>>);

impl DevKeysCell {
    fn with<R>(&self, f: impl FnOnce(&std::cell::RefCell<DevKeys>) -> R) -> R {
        let mut g = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let cell = std::cell::RefCell::new(g.take().unwrap_or_default());
        let r = f(&cell);
        *g = Some(cell.into_inner());
        r
    }
}

static DEV_KEYS: DevKeysCell = DevKeysCell(std::sync::Mutex::new(None));

fn just(k: KeyCode) -> bool {
    DEV_KEYS.with(|d| d.borrow().just.contains(&k))
}
fn held(k: KeyCode) -> bool {
    DEV_KEYS.with(|d| d.borrow().held.contains(&k))
}

#[allow(clippy::too_many_arguments)]
fn dev_input(
    time: Res<Time>,
    mut st: ResMut<DevState>,
    mut overlay: ResMut<crate::debug::DebugOverlay>,
    mut buttons: Query<(&Interaction, &DevButton, &mut BackgroundColor), (Changed<Interaction>, Without<DevTabButton>)>,
    mut tabs: Query<(&Interaction, &DevTabButton), Changed<Interaction>>,
    mut windows: Query<(&Window, &mut bevy::window::CursorOptions), With<bevy::window::PrimaryWindow>>,
    cams: Query<(&Camera, &GlobalTransform), With<FlyCam>>,
    ui_hover: Query<&Interaction>,
    sim: NonSend<Sim>,
) {
    let _ = &sim;
    let dt = time.delta_secs();
    if just(KeyCode::F10) {
        st.open = !st.open;
        st.rebuild = true;
        if std::env::var_os("P2GAME_DEV_TRACE").is_some() {
            println!("devmenu: {}", if st.open { "open" } else { "closed" });
        }
    }
    if just(KeyCode::F11) {
        // Leaving with time running: the world already runs, so don't toggle it back to paused.
        if !(st.editor && st.editor_time) {
            st.pending.push(Action::PauseWorld);
        }
        st.editor = !st.editor;
        st.editor_time = false;
        if st.editor {
            st.tab = Tab::Editor;
        }
        st.rebuild = true;
        let msg = if st.editor { "Editor: world paused (Space resumes). LMB select, RMB look." } else { "Editor closed" };
        st.toast(msg);
    }
    if just(KeyCode::Escape) && (st.open || st.editor) {
        if st.editor {
            if !st.editor_time {
                st.pending.push(Action::PauseWorld);
            }
            st.editor = false;
            st.editor_time = false;
        }
        st.open = false;
        st.rebuild = true;
    }
    if let Some(t) = st.toast.as_mut() {
        t.1 -= dt;
        if t.1 <= 0.0 {
            st.toast = None;
            st.rebuild = true;
        }
    }
    // Panel buttons.
    for (i, b, mut bg) in buttons.iter_mut() {
        match *i {
            Interaction::Pressed => {
                match b.0.clone() {
                    ButtonKind::Act(a) => st.pending.push(a),
                    ButtonKind::Render(m) => {
                        st.render = m;
                        st.toast(format!("Render: {}", m.label()));
                    }
                    ButtonKind::Overlay(f) => {
                        let v = f(&mut st.ov);
                        *v = !*v;
                    }
                    ButtonKind::StatsOverlay => overlay.visible = !overlay.visible,
                    ButtonKind::Editor => {
                        if !(st.editor && st.editor_time) {
                            st.pending.push(Action::PauseWorld);
                        }
                        st.editor = !st.editor;
                        st.editor_time = false;
                    }
                    ButtonKind::Gizmo(g) => st.gizmo_mode = g,
                    ButtonKind::Snap => st.snap = !st.snap,
                    ButtonKind::TogglePanel => st.panel_hidden = !st.panel_hidden,
                }
                st.rebuild = true;
            }
            Interaction::Hovered => bg.0 = BTN_HOVER,
            Interaction::None => {}
        }
    }
    for (i, t) in tabs.iter_mut() {
        if *i == Interaction::Pressed && st.tab != t.0 {
            st.tab = t.0;
            st.rebuild = true;
        }
    }
    // The OS cursor: free while the menu or editor is up.
    if let Ok((_, mut cur)) = windows.single_mut() {
        let free = st.open || st.editor;
        let want_vis = free || cur.visible;
        if free && !cur.visible {
            cur.visible = true;
            cur.grab_mode = bevy::window::CursorGrabMode::None;
        }
        let _ = want_vis;
    }
    // Editor keys.
    if st.editor {
        let over_ui = ui_hover.iter().any(|i| *i != Interaction::None);
        let ctrl = held(KeyCode::ControlLeft) || held(KeyCode::ControlRight);
        if just(KeyCode::KeyG) {
            st.gizmo_mode = GizmoMode::Move;
            st.rebuild = true;
        }
        if just(KeyCode::KeyR) {
            st.gizmo_mode = GizmoMode::Rotate;
            st.rebuild = true;
        }
        if just(KeyCode::Delete) {
            st.pending.push(Action::Delete);
        }
        if ctrl && just(KeyCode::KeyD) {
            st.pending.push(Action::Duplicate);
        }
        if just(KeyCode::End) {
            st.pending.push(Action::DropToFloor);
        }
        if just(KeyCode::KeyF) {
            st.pending.push(Action::TeleportToSelected);
        }
        let step = if held(KeyCode::ShiftLeft) { 64.0 } else { 16.0 };
        let nudges = [
            (KeyCode::ArrowUp, [1.0, 0.0, 0.0]),
            (KeyCode::ArrowDown, [-1.0, 0.0, 0.0]),
            (KeyCode::ArrowLeft, [0.0, -1.0, 0.0]),
            (KeyCode::ArrowRight, [0.0, 1.0, 0.0]),
            (KeyCode::PageUp, [0.0, 0.0, 1.0]),
            (KeyCode::PageDown, [0.0, 0.0, -1.0]),
        ];
        for (k, d) in nudges {
            if just(k) {
                match st.gizmo_mode {
                    GizmoMode::Move => {
                        // Arrow keys move relative to the camera's heading (snapped to an axis).
                        let d = cam_relative(&cams, d);
                        st.pending.push(Action::Nudge([d[0] * step, d[1] * step, d[2] * step]));
                    }
                    GizmoMode::Rotate => {
                        let a = if d[1] != 0.0 || d[0] != 0.0 { (d[1] + d[0]) as i32 } else { d[2] as i32 };
                        st.pending.push(Action::Rotate(a * if held(KeyCode::ShiftLeft) { 16384 } else { 2048 }));
                    }
                }
            }
        }
        // Click: grab a gizmo handle of the selection, else select.
        let (clicked, lmb_held) = DEV_KEYS.with(|k| {
            let k = k.borrow();
            (k.mouse_just.contains(&MouseButton::Left), k.mouse_held.contains(&MouseButton::Left))
        });
        if just(KeyCode::KeyH) {
            st.panel_hidden = !st.panel_hidden;
            st.rebuild = true;
        }
        if just(KeyCode::Space) {
            st.pending.push(Action::ToggleEditorTime);
        }
        let cursor = windows.single().ok().and_then(|(w, _)| w.cursor_position());
        let ray = match (cursor, cams.single()) {
            (Some(p), Ok((cam, ctf))) => cam.viewport_to_world(ctf, p).ok().map(|r| (r.origin, *r.direction)),
            _ => None,
        };
        let vm = &sim.game.world.vm;
        let sel = st.selected.filter(|&s| vm.objects.get(s).is_some_and(|o| !o.deleted));
        if !lmb_held {
            st.drag = None;
        }
        if let (Some(d), Some(_), Some((ro, rd)), Some(c)) = (st.drag, sel, ray, cursor) {
            match d.axis {
                Some(i) => {
                    if let Some(t) = closest_on_axis(d.start_loc, AXES[i], ro, rd) {
                        let p = d.start_loc + AXES[i] * (t - d.start_t);
                        st.pending.push(Action::SetLocation(to_uu(p)));
                    }
                }
                None => {
                    let yaw = d.start_yaw + ((c.x - d.start_cursor.x) * 90.0) as i32;
                    st.pending.push(Action::SetYaw(yaw));
                }
            }
        } else if clicked && !over_ui {
            let mut grabbed = false;
            if let (Some(s), Some((ro, rd)), Some(c), Ok((cam, ctf))) = (sel, ray, cursor, cams.single()) {
                let (loc, _, _, len) = gizmo_geom(vm, s);
                let screen = |p: Vec3| cam.world_to_viewport(ctf, p).ok();
                match st.gizmo_mode {
                    GizmoMode::Move => {
                        let best = (0..3)
                            .filter_map(|i| Some((i, seg_dist(c, screen(loc)?, screen(loc + AXES[i] * len)?))))
                            .filter(|(_, dist)| *dist < 12.0)
                            .min_by(|a, b| a.1.total_cmp(&b.1));
                        if let Some((i, _)) = best {
                            if let Some(t) = closest_on_axis(loc, AXES[i], ro, rd) {
                                st.drag = Some(Drag { axis: Some(i), start_loc: loc, start_t: t, start_yaw: 0, start_cursor: c });
                                grabbed = true;
                            }
                        }
                    }
                    GizmoMode::Rotate => {
                        // The ring (or anywhere within it) grabs: horizontal mouse travel turns it.
                        if rd.y.abs() > 1e-4 {
                            let s_t = (loc.y - ro.y) / rd.y;
                            let hit = ro + rd * s_t;
                            if s_t > 0.0 && hit.distance(loc) < len * 1.2 {
                                st.drag = Some(Drag { axis: None, start_loc: loc, start_t: 0.0, start_yaw: vm.actor_rot(s)[1], start_cursor: c });
                                grabbed = true;
                            }
                        }
                    }
                }
            }
            if !grabbed {
                if let Some((o, d)) = ray {
                    st.pending.push(Action::Pick(o, d));
                }
            }
        }
    }
}

/// Unreal-space direction for an arrow key: forward/back/left/right along the camera's yaw,
/// snapped to the nearest world axis (so nudges stay on a grid).
fn cam_relative(cams: &Query<(&Camera, &GlobalTransform), With<FlyCam>>, d: [f32; 3]) -> [f32; 3] {
    if d[2] != 0.0 {
        return d;
    }
    let Ok((_, t)) = cams.single() else { return d };
    let f = t.forward();
    // Bevy (x, z) -> Unreal (x, y).
    let (fx, fy) = (f.x, f.z);
    let (ax, ay) = if fx.abs() > fy.abs() { (fx.signum(), 0.0) } else { (0.0, fy.signum()) };
    // Unreal Y grows to the right of +X when seen from above (left-handed).
    let (rx, ry) = (-ay, ax);
    let (fw, rt) = (d[0], d[1]);
    [ax * fw + rx * rt, ay * fw + ry * rt, 0.0]
}

/// Picks the actor under a world-space ray (Bevy coordinates): the nearest visible actor whose
/// collision cylinder (or a small sphere for actors without one) the ray passes through.
fn pick(vm: &Vm, player: Option<ObjId>, origin: Vec3, dir: Vec3) -> Option<(f32, ObjId)> {
    let pawn = player.and_then(|pc| vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()));
    let mut best: Option<(f32, ObjId)> = None;
    for &a in &vm.actors {
        let o = &vm.objects[a];
        if o.deleted || Some(a) == pawn || Some(a) == player {
            continue;
        }
        if !pickable(vm, a) {
            continue;
        }
        let loc = to_bevy(vm.actor_loc(a));
        let r = (vm.get_prop(a, "CollisionRadius").map(|v| v.as_float()).unwrap_or(0.0) / UU_PER_M).max(0.25);
        let h = (vm.get_prop(a, "CollisionHeight").map(|v| v.as_float()).unwrap_or(0.0) / UU_PER_M).max(0.25);
        if let Some(t) = ray_cylinder(origin, dir, loc, r, h) {
            if t > 0.0 && best.is_none_or(|b| t < b.0) {
                best = Some((t, a));
            }
        }
    }
    best
}

/// Actors worth selecting: visible ones (meshes, static meshes, sprites of pickups) and pawns.
fn pickable(vm: &Vm, a: ObjId) -> bool {
    let c = vm.objects[a].class;
    if vm.class_is_a(c, "Controller") || vm.class_is_a(c, "Info") || vm.class_is_a(c, "Brush") && !vm.class_is_a(c, "Mover") {
        return false;
    }
    if vm.class_is_a(c, "Pawn") || vm.class_is_a(c, "Pickup") || vm.class_is_a(c, "KActor") || vm.class_is_a(c, "Mover") {
        return true;
    }
    if p2game::ai::render_hidden(vm, a) {
        return false;
    }
    matches!(vm.get_prop(a, "DrawType").map(|v| v.as_int()), Some(2) | Some(8))
        && vm.get_prop(a, "bStatic").is_none_or(|v| !v.as_bool())
        && vm.get_prop(a, "Base").and_then(|v| v.as_obj()).is_none()
}

/// Ray vs upright cylinder (Bevy Y up). Returns the entry distance.
fn ray_cylinder(o: Vec3, d: Vec3, c: Vec3, r: f32, h: f32) -> Option<f32> {
    let (ox, oz, dx, dz) = (o.x - c.x, o.z - c.z, d.x, d.z);
    let a = dx * dx + dz * dz;
    let mut ts: Vec<f32> = Vec::new();
    if a > 1e-8 {
        let b = 2.0 * (ox * dx + oz * dz);
        let cc = ox * ox + oz * oz - r * r;
        let disc = b * b - 4.0 * a * cc;
        if disc >= 0.0 {
            let s = disc.sqrt();
            for t in [(-b - s) / (2.0 * a), (-b + s) / (2.0 * a)] {
                let y = o.y + d.y * t;
                if (y - c.y).abs() <= h {
                    ts.push(t);
                }
            }
        }
    }
    if d.y.abs() > 1e-6 {
        for cap in [c.y - h, c.y + h] {
            let t = (cap - o.y) / d.y;
            let p = o + d * t;
            if (p.x - c.x).powi(2) + (p.z - c.z).powi(2) <= r * r {
                ts.push(t);
            }
        }
    }
    ts.into_iter().filter(|t| *t > 0.0).reduce(f32::min)
}

// ---------------------------------------------------------------------------------------------
// Actions (run with the VM)
// ---------------------------------------------------------------------------------------------

fn obj(vm: &Vm, o: ObjId, p: &str) -> Option<ObjId> {
    vm.get_prop(o, p).and_then(|v| v.as_obj()).filter(|&x| vm.objects.get(x).is_some_and(|x| !x.deleted))
}

fn apply_actions(mut st: ResMut<DevState>, mut sim: NonSendMut<Sim>, cams: Query<&GlobalTransform, With<FlyCam>>, mut trace_frame: Local<u32>) {
    let sim = &mut *sim;
    // New level: object ids changed.
    if st.level_serial != sim.game.level_serial {
        st.level_serial = sim.game.level_serial;
        st.selected = None;
        st.game_speed = 1.0;
        st.gravity_scale = 1.0;
        st.speed_scale = 1.0;
        st.jump_scale = 1.0;
        if st.editor && !st.editor_time {
            sim.paused = true;
        }
    }
    if st.game_speed == 0.0 {
        st.game_speed = 1.0;
        st.gravity_scale = 1.0;
        st.speed_scale = 1.0;
        st.jump_scale = 1.0;
    }
    let pending = std::mem::take(&mut st.pending);
    let Some(pc) = sim.game.player_controller else {
        return;
    };
    let host = sim.game.host.clone();
    let cam = cams.single().ok().map(|t| (t.translation(), t.forward().as_vec3()));
    for a in pending {
        let vm = &mut sim.game.world.vm;
        let pawn = obj(vm, pc, "Pawn");
        match a {
            Action::Exec(c) => run(vm, pc, c, &mut st),
            Action::God => {
                st.god = !st.god;
                vm.set_prop(pc, "bGodMode", Value::Bool(st.god));
                let msg = if st.god { "God mode ON" } else { "God mode off" };
                st.toast(msg);
            }
            Action::Ghost | Action::Fly => {
                let ghost = matches!(a, Action::Ghost);
                if let Some(p) = pawn {
                    vm.set_prop(p, "bCollideWorld", Value::Bool(!ghost));
                    set_collision(vm, p, !ghost);
                    vm.set_prop(p, "UnderWaterTime", Value::Float(-1.0));
                    vm.set_prop(pc, "bCheatFlying", Value::Bool(true));
                    let _ = vm.goto_state(pc, "PlayerFlying", "");
                    call(vm, p, "SetPhysics", vec![Value::Byte(4)]);
                    st.toast(if ghost { "Ghost: fly through walls (Walk to stop)" } else { "Fly mode (Walk to stop)" });
                }
            }
            Action::Walk => {
                if let Some(p) = pawn {
                    vm.set_prop(pc, "bCheatFlying", Value::Bool(false));
                    vm.set_prop(p, "bCollideWorld", Value::Bool(true));
                    set_collision(vm, p, true);
                    call(vm, p, "SetPhysics", vec![Value::Byte(2)]);
                    if vm.call_event(pc, "ClientReStart", vec![]).is_err() {
                        let _ = vm.goto_state(pc, "PlayerWalking", "");
                    }
                    st.toast("Walking");
                }
            }
            Action::FullHealth => {
                if let Some(p) = pawn {
                    let hm = vm.get_prop(p, "HealthMax").map(|v| v.as_float()).unwrap_or(100.0).max(100.0);
                    vm.set_prop(p, "Health", Value::Int(hm as i32));
                    st.toast("Health restored");
                }
            }
            Action::InfiniteAmmo => {
                st.infinite_ammo = !st.infinite_ammo;
                let msg = if st.infinite_ammo { "Infinite ammo ON" } else { "Infinite ammo off" };
                st.toast(msg);
            }
            Action::KillPawns => {
                let pawns: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "Pawn") && Some(a) != pawn).collect();
                let mut n = 0;
                for p in pawns {
                    if vm.get_prop(p, "Health").is_some_and(|h| h.as_float() > 0.0) {
                        damage(vm, p, pawn, 10000, "Crushed");
                        n += 1;
                    }
                }
                st.toast(format!("Killed {n} pawns"));
            }
            Action::FreezeAi => {
                st.freeze_ai = !st.freeze_ai;
                let ctrls: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && a != pc && vm.class_is_a(vm.objects[a].class, "AIController")).collect();
                for c in ctrls {
                    vm.set_prop(c, "bStasis", Value::Bool(st.freeze_ai));
                    if let Some(p) = obj(vm, c, "Pawn") {
                        vm.set_prop(p, "bStasis", Value::Bool(st.freeze_ai));
                        if st.freeze_ai {
                            vm.set_prop(p, "Acceleration", Value::Vector([0.0; 3]));
                            vm.set_prop(p, "Velocity", Value::Vector([0.0; 3]));
                        }
                    }
                }
                let msg = if st.freeze_ai { "AI frozen" } else { "AI running" };
                st.toast(msg);
            }
            Action::GameSpeed(v) => {
                st.game_speed = v;
                if let Some(li) = vm.level {
                    vm.set_prop(li, "TimeDilation", Value::Float(v));
                }
                st.toast(format!("Game speed {v}x"));
            }
            Action::Gravity(v) => {
                st.gravity_scale = v;
                // Every PhysicsVolume (the level's default one included).
                let vols: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "PhysicsVolume")).collect();
                for vol in vols {
                    let base = vm.default_value(vm.objects[vol].class, "Gravity").map(|g| g.as_vec()).unwrap_or([0.0, 0.0, -950.0]);
                    vm.set_prop(vol, "Gravity", Value::Vector([base[0] * v, base[1] * v, base[2] * v]));
                }
                // Physics keeps its own copy of each volume's gravity (read at load).
                set_physics_gravity(v);
                st.toast(format!("Gravity {v}x"));
            }
            Action::Speed(v) => {
                st.speed_scale = v;
                if let Some(p) = pawn {
                    for k in ["GroundSpeed", "WaterSpeed", "AirSpeed", "AccelRate"] {
                        if let Some(d) = vm.default_value(vm.objects[p].class, k).map(|x| x.as_float()) {
                            vm.set_prop(p, k, Value::Float(d * v));
                        }
                    }
                }
                st.toast(format!("Run speed {v}x"));
            }
            Action::Jump(v) => {
                st.jump_scale = v;
                if let Some(p) = pawn {
                    let d = vm.default_value(vm.objects[p].class, "JumpZ").map(|x| x.as_float()).unwrap_or(550.0);
                    vm.set_prop(p, "JumpZ", Value::Float(d * v));
                }
                st.toast(format!("Jump {v}x"));
            }
            Action::Summon(class) => {
                let Some(at) = crosshair(vm, &host, pc, cam, 72.0) else { continue };
                // Stand it on the floor under the crosshair point (pawns spawned in mid air at a
                // wall hang there until their physics settles).
                let h = vm.find_class_by_name(class).ok().and_then(|c| vm.default_value(c, "CollisionHeight").map(|v| v.as_float())).unwrap_or(72.0);
                let down = uscript::host::TraceRequest { start: at, end: [at[0], at[1], at[2] - 4000.0], extent: [0.0; 3], trace_actors: false };
                let floor = host.world.borrow().clone().and_then(|w| w.trace(vm, pawn, &down)).map(|h| h.location[2]).unwrap_or(at[2]);
                match spawn(vm, class, [at[0], at[1], floor + h + 2.0], pc) {
                    Some(a) => {
                        st.toast(format!("Spawned {}", vm.object_name(a)));
                        if st.editor {
                            st.selected = Some(a);
                        }
                    }
                    None => st.toast(format!("Could not spawn {class}")),
                }
            }
            Action::Explode => {
                let Some(at) = crosshair(vm, &host, pc, cam, 24.0) else { continue };
                match spawn(vm, "FX.GrenadeExplosion", at, pc) {
                    Some(e) => {
                        vm.set_prop(e, "Instigator", Value::Object(pawn));
                        st.toast("Boom");
                    }
                    None => st.toast("Explosion class not available"),
                }
            }
            Action::TeleportCrosshair => {
                let (Some(p), Some(at)) = (pawn, crosshair(vm, &host, pc, cam, 60.0)) else { continue };
                let h = vm.get_prop(p, "CollisionHeight").map(|v| v.as_float()).unwrap_or(72.0);
                if host.move_actor(vm, p, [at[0], at[1], at[2] + h + 4.0], false) {
                    vm.set_prop(p, "Velocity", Value::Vector([0.0; 3]));
                    st.toast("Teleported");
                } else {
                    st.toast("No room there");
                }
            }
            Action::Launch => {
                if let Some(p) = pawn {
                    let r = vm.get_prop(pc, "Rotation").map(|v| v.as_rot()).unwrap_or_default();
                    let yaw = r[1] as f32 * std::f32::consts::TAU / 65536.0;
                    vm.set_prop(p, "Velocity", Value::Vector([yaw.cos() * 900.0, yaw.sin() * 900.0, 1600.0]));
                    call(vm, p, "SetPhysics", vec![Value::Byte(2)]);
                    st.toast("Wheee");
                }
            }
            Action::PauseWorld => {
                // In the editor the world holds; outside it this toggles.
                if st.editor {
                    st.editor_time = false;
                    if !sim.paused {
                        st.was_paused = false;
                        sim.paused = true;
                    }
                } else if st.was_paused {
                    sim.paused = !sim.paused;
                    st.was_paused = sim.paused;
                } else {
                    // Leaving the editor (or the World tab's button).
                    sim.paused = !sim.paused && !st.open_after_editor();
                    st.was_paused = sim.paused;
                }
                st.rebuild = true;
            }
            Action::ToggleEditorTime => {
                if st.editor {
                    st.editor_time = !st.editor_time;
                    sim.paused = !st.editor_time;
                    let msg = if st.editor_time { "Time running (Space freezes)" } else { "Time frozen (Space resumes)" };
                    st.toast(msg);
                    st.rebuild = true;
                }
            }
            Action::Pick(o, d) => {
                // The world trace finds the real mesh under the cursor; the cylinder test catches
                // pawns and pickups that don't block traces, when they are nearer.
                let start = [o.x * UU_PER_M, o.z * UU_PER_M, o.y * UU_PER_M];
                let end = [start[0] + d.x * 30000.0, start[1] + d.z * 30000.0, start[2] + d.y * 30000.0];
                let req = uscript::host::TraceRequest { start, end, extent: [0.0; 3], trace_actors: true };
                let hit = host.world.borrow().clone().and_then(|w| w.trace(vm, pawn, &req));
                let trace_hit = hit.as_ref().and_then(|h| {
                    let a = h.actor?;
                    let dist = Vec3::from(h.location).distance(Vec3::from(start)) / UU_PER_M;
                    (Some(a) != vm.level && !vm.objects[a].deleted && Some(a) != pawn).then_some((dist, a))
                });
                let cyl = pick(vm, Some(pc), o, d);
                let sel = match (trace_hit, cyl) {
                    (Some((td, ta)), Some((cd, ca))) => Some(if cd < td + 0.5 { ca } else { ta }),
                    (Some((_, a)), None) | (None, Some((_, a))) => Some(a),
                    (None, None) => None,
                };
                st.selected = sel;
                st.inspector.clear();
                st.rebuild = true;
                match sel {
                    Some(a) => {
                        if std::env::var_os("P2GAME_DEV_TRACE").is_some() {
                            println!("devmenu: picked {} at {:?}", vm.object_name(a), vm.actor_loc(a));
                        }
                        st.toast(format!("Selected {}", vm.object_name(a)))
                    }
                    None => st.toast("Nothing there"),
                }
            }
            Action::Select(s) => {
                st.selected = s;
                st.inspector.clear();
                st.rebuild = true;
                if let Some(a) = s {
                    st.toast(format!("Selected {}", vm.object_name(a)));
                }
            }
            Action::Nudge(d) => {
                if let Some(s) = st.selected {
                    let l = vm.actor_loc(s);
                    let mut to = [l[0] + d[0], l[1] + d[1], l[2] + d[2]];
                    if st.snap {
                        to = to.map(|v| (v / 16.0).round() * 16.0);
                    }
                    place(vm, &host, s, to);
                    st.inspector.clear();
                }
            }
            Action::SetLocation(to) => {
                if let Some(s) = st.selected {
                    let to = if st.snap { to.map(|v| (v / 16.0).round() * 16.0) } else { to };
                    place(vm, &host, s, to);
                    st.inspector.clear();
                }
            }
            Action::SetYaw(yaw) => {
                if let Some(s) = st.selected {
                    let r = vm.actor_rot(s);
                    let yaw = if st.snap { (yaw + 1024) & !2047 } else { yaw };
                    let r = [r[0], yaw & 0xffff, r[2]];
                    vm.set_prop(s, "Rotation", Value::Rotator(r));
                    if vm.class_is_a(vm.objects[s].class, "Pawn") {
                        vm.set_prop(s, "DesiredRotation", Value::Rotator(r));
                    }
                    st.inspector.clear();
                }
            }
            Action::Rotate(yaw) => {
                if let Some(s) = st.selected {
                    let r = vm.actor_rot(s);
                    let r = [r[0], (r[1] + yaw) & 0xffff, r[2]];
                    vm.set_prop(s, "Rotation", Value::Rotator(r));
                    if vm.class_is_a(vm.objects[s].class, "Pawn") {
                        vm.set_prop(s, "DesiredRotation", Value::Rotator(r));
                    }
                    st.inspector.clear();
                }
            }
            Action::Delete => {
                if let Some(s) = st.selected.take() {
                    let n = vm.object_name(s);
                    if vm.class_is_a(vm.objects[s].class, "Pawn") {
                        if let Some(c) = obj(vm, s, "Controller") {
                            vm.destroy_actor(c);
                        }
                    }
                    if vm.destroy_actor(s) {
                        st.toast(format!("Deleted {n}"));
                    } else {
                        st.toast(format!("{n} is static (can't delete)"));
                    }
                    st.inspector.clear();
                }
            }
            Action::Duplicate => {
                if let Some(s) = st.selected {
                    let class = vm.objects[s].class;
                    let name = format!("{}.{}", vm.classes[class].package, vm.classes[class].name);
                    let l = vm.actor_loc(s);
                    let r = (vm.get_prop(s, "CollisionRadius").map(|v| v.as_float()).unwrap_or(16.0) * 2.0).max(32.0);
                    match spawn(vm, &name, [l[0] + r, l[1], l[2]], pc) {
                        Some(d) => {
                            for k in ["Rotation", "DrawScale", "DrawScale3D", "StaticMesh", "Skins", "Mesh"] {
                                if let Some(v) = vm.get_prop(s, k).cloned() {
                                    vm.set_prop(d, k, v);
                                }
                            }
                            st.selected = Some(d);
                            st.toast(format!("Duplicated as {}", vm.object_name(d)));
                        }
                        None => st.toast(format!("Can't duplicate {}", vm.classes[class].name)),
                    }
                    st.inspector.clear();
                }
            }
            Action::DropToFloor => {
                if let Some(s) = st.selected {
                    let l = vm.actor_loc(s);
                    let h = vm.get_prop(s, "CollisionHeight").map(|v| v.as_float()).unwrap_or(0.0);
                    let req = uscript::host::TraceRequest { start: l, end: [l[0], l[1], l[2] - 4096.0], extent: [0.0; 3], trace_actors: false };
                    if let Some(hit) = host.world.borrow().clone().and_then(|w| w.trace(vm, Some(s), &req)) {
                        place(vm, &host, s, [l[0], l[1], hit.location[2] + h + 1.0]);
                        st.toast("Dropped to floor");
                    }
                    st.inspector.clear();
                }
            }
            Action::TeleportToSelected => {
                if let (Some(s), Some(p)) = (st.selected, pawn) {
                    let l = vm.actor_loc(s);
                    let r = vm.get_prop(s, "CollisionRadius").map(|v| v.as_float()).unwrap_or(32.0) + 80.0;
                    let ok = (0..8).any(|k| {
                        let a = k as f32 * std::f32::consts::FRAC_PI_4;
                        host.move_actor(vm, p, [l[0] + a.cos() * r, l[1] + a.sin() * r, l[2] + 20.0], false)
                    });
                    st.toast(if ok { "Moved player next to selection" } else { "No room next to selection" });
                    if ok {
                        FOCUS.with(|f| f.set(Some(to_bevy(l))));
                    }
                } else if let Some(s) = st.selected {
                    FOCUS.with(|f| f.set(Some(to_bevy(vm.actor_loc(s)))));
                }
            }
            Action::BringSelected => {
                if let Some(s) = st.selected {
                    if let Some(at) = crosshair(vm, &host, pc, cam, 60.0) {
                        let h = vm.get_prop(s, "CollisionHeight").map(|v| v.as_float()).unwrap_or(0.0);
                        place(vm, &host, s, [at[0], at[1], at[2] + h + 2.0]);
                        st.inspector.clear();
                    }
                }
            }
            Action::Possess => {
                if let Some(s) = st.selected.filter(|&s| vm.class_is_a(vm.objects[s].class, "Pawn")) {
                    if let Some(old) = obj(vm, s, "Controller") {
                        vm.destroy_actor(old);
                    }
                    if let Err(e) = vm.call_event(pc, "Possess", vec![Value::Object(Some(s))]) {
                        vm.record_error("devmenu Possess", &e);
                    }
                    st.toast(format!("Now playing as {}", vm.object_name(s)));
                } else {
                    st.toast("Select a pawn to possess");
                }
            }
            Action::Kill => {
                if let Some(s) = st.selected {
                    damage(vm, s, pawn, 10000, "Crushed");
                    st.inspector.clear();
                }
            }
        }
    }
    // Continuous effects.
    let vm = &mut sim.game.world.vm;
    if st.god {
        vm.set_prop(pc, "bGodMode", Value::Bool(true));
    }
    if st.infinite_ammo {
        if let Some(w) = obj(vm, pc, "Pawn").and_then(|p| obj(vm, p, "Weapon")) {
            if let Some(am) = obj(vm, w, "AmmoType") {
                let max = vm.get_prop(am, "MaxAmmo").map(|v| v.as_int()).unwrap_or(999).max(1);
                vm.set_prop(am, "AmmoAmount", Value::Int(max));
            }
        }
    }
    if st.editor && !st.editor_time && !sim.paused {
        sim.paused = true;
    }
    // Frozen world: the world tick (which places bone attachments) doesn't run, so an actor the
    // editor moves would leave its head, hat and weapon behind. Re-place attachments here.
    if sim.paused && st.editor {
        let host = sim.game.host.clone();
        host.update_attachments(&mut sim.game.world.vm);
    }
    // QA: with P2GAME_DEV_TRACE, the world clock every 30 frames while the editor is up (it only
    // advances when time runs).
    if st.editor && std::env::var_os("P2GAME_DEV_TRACE").is_some() && *trace_frame % 30 == 0 {
        let vm = &sim.game.world.vm;
        let lt = vm.level.and_then(|l| vm.get_prop(l, "TimeSeconds")).map(|v| v.as_float());
        println!("devmenu clock: editor_time {} paused {} level_time {lt:?}", st.editor_time, sim.paused);
    }
    *trace_frame = trace_frame.wrapping_add(1);
}

impl DevState {
    fn open_after_editor(&self) -> bool {
        false
    }
}

/// Point the editor camera should look at next frame ("go to"). Shared across worker threads.
struct FocusCell(std::sync::Mutex<Option<Vec3>>);

impl FocusCell {
    fn with<R>(&self, f: impl FnOnce(&std::cell::Cell<Option<Vec3>>) -> R) -> R {
        let mut g = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let c = std::cell::Cell::new(*g);
        let r = f(&c);
        *g = c.get();
        r
    }
}

static FOCUS: FocusCell = FocusCell(std::sync::Mutex::new(None));

fn run(vm: &mut Vm, pc: ObjId, c: &str, st: &mut DevState) {
    // P2CheatManager cheats check bAllowCManager and warn the first time; the dev menu enables
    // cheats for the session the way the "Sissy" console command does.
    if let Some(g) = vm.level.and_then(|l| obj(vm, l, "Game")) {
        vm.set_prop(g, "bAllowCManager", Value::Bool(true));
        vm.set_prop(g, "bWarnedCheater", Value::Bool(true));
    }
    let ok = crate::frontend::exec_command(vm, pc, c);
    st.toast(if ok { format!("> {c}") } else { format!("> {c} (console)") });
}

/// Scales gravity in the physics world: the level default volume and every PhysicsVolume,
/// relative to the values loaded with the map (remembered the first time).
fn set_physics_gravity(scale: f32) {
    thread_local!(static BASE: std::cell::RefCell<Option<(usize, Vec<(p2physics::ActorId, bevy::math::Vec3)>, bevy::math::Vec3)>> = const { std::cell::RefCell::new(None) });
    let Some(ph) = p2game::physics::current() else { return };
    let mut w = ph.world.borrow_mut();
    let key = std::rc::Rc::as_ptr(&ph) as usize;
    BASE.with(|b| {
        let mut b = b.borrow_mut();
        if b.as_ref().is_none_or(|x| x.0 != key) {
            let vols = w.ids().into_iter().filter_map(|id| w.get(id).and_then(|a| a.volume.as_ref()).map(|v| (id, v.gravity))).collect();
            *b = Some((key, vols, w.default_volume_info.gravity));
        }
        let (_, vols, def) = b.as_ref().unwrap();
        w.default_volume_info.gravity = *def * scale;
        for (id, g) in vols {
            if let Some(v) = w.get_mut(*id).and_then(|a| a.volume.as_mut()) {
                v.gravity = *g * scale;
            }
        }
    });
}

fn call(vm: &mut Vm, a: ObjId, f: &str, args: Vec<Value>) {
    if let Err(e) = vm.call_event(a, f, args) {
        vm.record_error(&format!("devmenu {f}"), &e);
    }
}

fn set_collision(vm: &mut Vm, a: ObjId, on: bool) {
    call(vm, a, "SetCollision", vec![Value::Bool(on), Value::Bool(on), Value::Bool(on)]);
}

fn damage(vm: &mut Vm, victim: ObjId, by: Option<ObjId>, amount: i32, dtype: &str) {
    let class = vm.find_class_by_name(dtype).ok();
    let loc = vm.actor_loc(victim);
    let args = vec![Value::Int(amount), Value::Object(by), Value::Vector(loc), Value::Vector([0.0, 0.0, 20000.0]), Value::Class(class)];
    call(vm, victim, "TakeDamage", args);
}

/// Spawns an actor of a class by name with begin-play events; pawns get their default controller
/// (`SpawnDefaultController`, as `P2CheatManager.Summon`'s AddController does).
fn spawn(vm: &mut Vm, class: &str, at: [f32; 3], pc: ObjId) -> Option<ObjId> {
    let c = vm.find_class_by_name(class).ok()?;
    let yaw = vm.get_prop(pc, "Rotation").map(|v| v.as_rot()[1]).unwrap_or(0);
    let p = uscript::host::SpawnParams { location: at, rotation: [0, (yaw + 32768) & 0xffff, 0], ..Default::default() };
    let a = vm.spawn_actor(c, p, true)?;
    if vm.class_is_a(c, "Pawn") && obj(vm, a, "Controller").is_none() {
        call(vm, a, "SpawnDefaultController", vec![]);
        // FPSPawn initialises its AI from the controller's Possess; give it a moment of life.
        if let Some(ctrl) = obj(vm, a, "Controller") {
            let _ = vm.goto_state(ctrl, "Auto", "");
        }
    }
    Some(a)
}

/// Where the crosshair points (camera ray against the world), pulled back `back` units.
fn crosshair(vm: &mut Vm, host: &p2game::host::GameHost, pc: ObjId, cam: Option<(Vec3, Vec3)>, back: f32) -> Option<[f32; 3]> {
    let (o, d) = match cam {
        Some((p, f)) => (p * UU_PER_M, f),
        None => return None,
    };
    // Bevy (x, y up, z) -> Unreal (x, z->y, y->z).
    let start = [o.x, o.z, o.y];
    let dir = [d.x, d.z, d.y];
    let end = [start[0] + dir[0] * 20000.0, start[1] + dir[1] * 20000.0, start[2] + dir[2] * 20000.0];
    let pawn = obj(vm, pc, "Pawn");
    let req = uscript::host::TraceRequest { start, end, extent: [0.0; 3], trace_actors: true };
    let w = host.world.borrow().clone()?;
    let hit = w.trace(vm, pawn, &req);
    let p = hit.map(|h| h.location).unwrap_or([start[0] + dir[0] * 800.0, start[1] + dir[1] * 800.0, start[2] + dir[2] * 800.0]);
    Some([p[0] - dir[0] * back, p[1] - dir[1] * back, p[2] - dir[2] * back])
}

/// Moves an actor without sweeping (the editor places things where it's told), zeroing motion.
fn place(vm: &mut Vm, host: &p2game::host::GameHost, a: ObjId, to: [f32; 3]) {
    if !host.move_actor(vm, a, to, false) {
        vm.set_prop(a, "Location", Value::Vector(to));
    }
    vm.set_prop(a, "Velocity", Value::Vector([0.0; 3]));
}

// ---------------------------------------------------------------------------------------------
// Editor camera
// ---------------------------------------------------------------------------------------------

fn editor_camera(time: Res<Time>, st: Res<DevState>, mut ui: ResMut<crate::ui::UiInput>, mut q: Query<(&mut Transform, &mut FlyCam)>, mut was: Local<bool>) {
    if !st.editor {
        *was = false;
        return;
    }
    // The editor owns the camera: the script view and the fly camera's own input stay off.
    ui.game_look = false;
    ui.player_view = false;
    ui.captured = true;
    let Ok((mut tf, mut cam)) = q.single_mut() else { return };
    if !*was {
        // Entering: step back out of the Dude's head (his own body is drawn in the editor).
        *was = true;
        let f = tf.forward().as_vec3();
        tf.translation += -Vec3::new(f.x, 0.0, f.z).normalize_or_zero() * 2.5 + Vec3::Y * 0.8;
        cam.yaw = (-f.z).atan2(f.x);
        cam.pitch = (f.y.asin() - 0.2).clamp(-1.5, 1.5);
        tf.rotation = crate::window::cam_rot(cam.yaw, cam.pitch);
    }
    if let Some(p) = FOCUS.with(|f| f.take()) {
        let back = tf.forward().as_vec3() * -6.0;
        tf.translation = p + back + Vec3::Y * 1.5;
        tf.look_at(p, Vec3::Y);
        let f = tf.forward();
        cam.yaw = (-f.z).atan2(f.x);
        cam.pitch = f.y.asin();
    }
    let (rmb, motion) = DEV_KEYS.with(|k| {
        let k = k.borrow();
        (k.mouse_held.contains(&MouseButton::Right), k.motion)
    });
    if rmb {
        cam.yaw -= motion.x * 0.003;
        cam.pitch = (cam.pitch - motion.y * 0.003).clamp(-1.54, 1.54);
        tf.rotation = crate::window::cam_rot(cam.yaw, cam.pitch);
    }
    let (fwd, right) = (*tf.forward(), *tf.right());
    let mut v = Vec3::ZERO;
    for (k, d) in [(KeyCode::KeyW, fwd), (KeyCode::KeyS, -fwd), (KeyCode::KeyD, right), (KeyCode::KeyA, -right), (KeyCode::KeyE, Vec3::Y), (KeyCode::KeyQ, -Vec3::Y)] {
        if held(k) {
            v += d;
        }
    }
    let speed = if held(KeyCode::ShiftLeft) { 40.0 } else { 9.0 };
    tf.translation += v.normalize_or_zero() * speed * time.delta_secs();
}

// ---------------------------------------------------------------------------------------------
// Render modes
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct RenderBackup {
    /// Material handle -> (unlit, base_color_texture) before the mode changed them.
    saved: std::collections::HashMap<AssetId<StandardMaterial>, (bool, Option<Handle<Image>>, Color)>,
    /// Baked (lightmapped) materials: (base texture, base colour, lightmap).
    baked: std::collections::HashMap<AssetId<p2view::baked::BakedMaterial>, (Option<Handle<Image>>, Color, Handle<Image>)>,
    /// A 1x1 lightmap of 128 (= full brightness in modulate-2x) for fullbright.
    flat: Option<Handle<Image>>,
    fog: Option<(Entity, DistanceFog)>,
}

fn style_material(m: &mut StandardMaterial, mode: RenderMode) {
    match mode {
        RenderMode::Fullbright => m.unlit = true,
        RenderMode::Untextured => {
            m.base_color_texture = None;
            m.base_color = Color::srgb(0.72, 0.70, 0.66);
        }
        RenderMode::Wireframe => {
            // Faces dark so the wire stands out.
            m.unlit = true;
            m.base_color_texture = None;
            m.base_color = Color::srgb(0.03, 0.035, 0.04);
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_render_modes(
    mut st: ResMut<DevState>,
    mut wire: ResMut<WireframeConfig>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut baked: ResMut<Assets<p2view::baked::BakedMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut backup: Local<RenderBackup>,
    mut commands: Commands,
    fogs: Query<(Entity, &DistanceFog), With<FlyCam>>,
    nofog: Query<Entity, (With<FlyCam>, Without<DistanceFog>)>,
) {
    // Fog toggle.
    if st.ov.no_fog {
        if let Ok((e, f)) = fogs.single() {
            backup.fog = Some((e, f.clone()));
            commands.entity(e).remove::<DistanceFog>();
        }
    } else if let Some((e, f)) = backup.fog.take() {
        if nofog.get(e).is_ok() {
            commands.entity(e).insert(f);
        }
    }
    // The first-person weapon overlay is hidden in hide_fp_weapon.
    let mode = st.render;
    let wanted_wire = matches!(mode, RenderMode::Wireframe | RenderMode::WireOverlay);
    if wire.global != wanted_wire {
        wire.global = wanted_wire;
    }
    // Materials: re-apply every second while a mode is on (new rigs and props appear).
    thread_local!(static TICK: std::cell::Cell<u32> = const { std::cell::Cell::new(0) });
    let t = TICK.with(|t| {
        t.set(t.get().wrapping_add(1));
        t.get()
    });
    if mode == st.applied_render && (mode == RenderMode::Lit || t % 60 != 0) {
        return;
    }
    // Restore everything first.
    if st.applied_render != mode {
        for (id, (unlit, tex, color)) in backup.saved.drain() {
            if let Some(mut m) = mats.get_mut(id) {
                m.unlit = unlit;
                m.base_color_texture = tex;
                m.base_color = color;
            }
        }
        for (id, (tex, color, lm)) in backup.baked.drain() {
            if let Some(mut m) = baked.get_mut(id) {
                m.base.base_color_texture = tex;
                m.base.base_color = color;
                m.extension.lightmap = lm;
            }
        }
    }
    st.applied_render = mode;
    if matches!(mode, RenderMode::Lit | RenderMode::WireOverlay) {
        return;
    }
    let ids: Vec<AssetId<StandardMaterial>> = mats.ids().collect();
    for id in ids {
        let Some(mut m) = mats.get_mut(id) else { continue };
        backup.saved.entry(id).or_insert((m.unlit, m.base_color_texture.clone(), m.base_color));
        style_material(&mut m, mode);
    }
    // Lightmapped BSP and baked static meshes: their shader multiplies by the lightmap (or vertex
    // light) and ignores `unlit`; fullbright swaps in a neutral lightmap.
    let flat = backup
        .flat
        .get_or_insert_with(|| {
            images.add(Image::new_fill(
                bevy::render::render_resource::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                bevy::render::render_resource::TextureDimension::D2,
                &[128, 128, 128, 255],
                bevy::render::render_resource::TextureFormat::Rgba8Unorm,
                bevy::asset::RenderAssetUsages::RENDER_WORLD,
            ))
        })
        .clone();
    let ids: Vec<AssetId<p2view::baked::BakedMaterial>> = baked.ids().collect();
    for id in ids {
        let Some(mut m) = baked.get_mut(id) else { continue };
        backup.baked.entry(id).or_insert((m.base.base_color_texture.clone(), m.base.base_color, m.extension.lightmap.clone()));
        match mode {
            RenderMode::Fullbright => m.extension.lightmap = flat.clone(),
            RenderMode::Untextured => {
                m.base.base_color_texture = None;
                m.base.base_color = Color::srgb(0.72, 0.70, 0.66);
            }
            RenderMode::Wireframe => {
                m.base.base_color_texture = None;
                m.base.base_color = Color::srgb(0.03, 0.035, 0.04);
                m.extension.lightmap = flat.clone();
            }
            _ => {}
        }
    }
}

/// The first-person weapon overlay: hidden on request and always in the editor.
fn hide_fp_weapon(st: Res<DevState>, mut fp: Query<&mut Camera, With<crate::weapons::FpCamera>>) {
    if st.ov.hide_hud_weapon || st.editor {
        for mut c in fp.iter_mut() {
            if c.is_active {
                c.is_active = false;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Overlays (gizmos)
// ---------------------------------------------------------------------------------------------

fn uu(p: [f32; 3]) -> Vec3 {
    to_bevy(p)
}

fn draw_cylinder(g: &mut Gizmos, c: Vec3, r: f32, h: f32, color: Color) {
    let iso_top = Isometry3d::new(c + Vec3::Y * h, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2));
    let iso_bot = Isometry3d::new(c - Vec3::Y * h, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2));
    g.circle(iso_top, r, color).resolution(20);
    g.circle(iso_bot, r, color).resolution(20);
    for k in 0..4 {
        let a = k as f32 * std::f32::consts::FRAC_PI_2;
        let o = Vec3::new(a.cos() * r, 0.0, a.sin() * r);
        g.line(c + o - Vec3::Y * h, c + o + Vec3::Y * h, color);
    }
}

fn draw_overlays(st: Res<DevState>, sim: NonSend<Sim>, mut g: Gizmos, cams: Query<&GlobalTransform, With<FlyCam>>, cams_full: Query<(&Camera, &GlobalTransform), With<FlyCam>>) {
    let vm = &sim.game.world.vm;
    let cam = cams.single().map(|t| t.translation()).unwrap_or(Vec3::ZERO);
    let near = |p: Vec3, d: f32| p.distance_squared(cam) < d * d;
    let ov = st.ov;
    let any = ov.bounds || ov.collision || ov.nav || ov.sight || ov.volumes || ov.axes || st.editor;
    if !any {
        return;
    }
    let player_pawn = sim.game.player_controller.and_then(|pc| obj(vm, pc, "Pawn"));
    if ov.axes || st.editor {
        g.grid(Isometry3d::new(Vec3::new((cam.x / 4.0).round() * 4.0, floor_y(vm, player_pawn, cam), (cam.z / 4.0).round() * 4.0), Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)), UVec2::splat(40), Vec2::splat(16.0 / UU_PER_M * 4.0), Color::srgba(1.0, 1.0, 1.0, 0.06));
        // Orientation widget in the bottom-left corner (it sat mid-screen, in the way): a small
        // world-axis triad placed just in front of the camera under that screen point.
        if let (Ok((cam_c, ctf)), Some(vp)) = (cams_full.single(), cams_full.single().ok().and_then(|(c, _)| c.logical_viewport_size())) {
            if let Ok(ray) = cam_c.viewport_to_world(ctf, Vec2::new(70.0, vp.y - 90.0)) {
                let o = ray.origin + *ray.direction * 3.0;
                g.arrow(o, o + Vec3::X * 0.25, Color::srgb(1.0, 0.2, 0.2));
                g.arrow(o, o + Vec3::Z * 0.25, Color::srgb(0.2, 1.0, 0.2));
                g.arrow(o, o + Vec3::Y * 0.25, Color::srgb(0.3, 0.5, 1.0));
            }
        }
    }
    for &a in &vm.actors {
        let o = &vm.objects[a];
        if o.deleted {
            continue;
        }
        let c = o.class;
        let loc = uu(vm.actor_loc(a));
        if !near(loc, 120.0) {
            continue;
        }
        let r = vm.get_prop(a, "CollisionRadius").map(|v| v.as_float()).unwrap_or(0.0) / UU_PER_M;
        let h = vm.get_prop(a, "CollisionHeight").map(|v| v.as_float()).unwrap_or(0.0) / UU_PER_M;
        let is_pawn = vm.class_is_a(c, "Pawn");
        if ov.collision && vm.get_prop(a, "bCollideActors").is_some_and(|v| v.as_bool()) && r > 0.0 && Some(a) != player_pawn {
            let col = if vm.get_prop(a, "bBlockActors").is_some_and(|v| v.as_bool()) { Color::srgb(1.0, 0.55, 0.1) } else { Color::srgb(0.9, 0.9, 0.3) };
            draw_cylinder(&mut g, loc, r, h, col);
        }
        if ov.bounds && pickable(vm, a) && Some(a) != player_pawn {
            let col = if is_pawn { Color::srgb(1.0, 0.3, 0.3) } else if vm.class_is_a(c, "Pickup") { Color::srgb(0.3, 1.0, 0.4) } else { Color::srgb(0.4, 0.7, 1.0) };
            let half = Vec3::new(r.max(0.2), h.max(0.2), r.max(0.2));
            g.cube(Transform::from_translation(loc).with_scale(half * 2.0), col);
        }
        if ov.volumes && (vm.class_is_a(c, "Triggers") || vm.class_is_a(c, "Trigger") || vm.class_is_a(c, "Volume")) && r > 0.0 {
            draw_cylinder(&mut g, loc, r, h.max(0.1), Color::srgba(0.8, 0.3, 1.0, 0.7));
        }
        if ov.nav && vm.class_is_a(c, "NavigationPoint") {
            g.sphere(Isometry3d::from_translation(loc), 0.15, Color::srgb(0.2, 0.9, 1.0)).resolution(8);
            if let Some(Value::Array(paths)) = vm.get_prop(a, "PathList") {
                for spec in paths.iter().filter_map(|v| v.as_obj()) {
                    if let Some(e) = obj(vm, spec, "End") {
                        let el = uu(vm.actor_loc(e));
                        let flags = vm.get_prop(spec, "reachFlags").map(|v| v.as_int()).unwrap_or(0);
                        let col = if flags & 16 != 0 { Color::srgba(1.0, 0.8, 0.2, 0.6) } else { Color::srgba(0.2, 0.9, 1.0, 0.45) };
                        g.line(loc, el, col);
                    }
                }
            }
        }
        if ov.sight && is_pawn && Some(a) != player_pawn && vm.get_prop(a, "Health").is_some_and(|v| v.as_float() > 0.0) {
            let rot = vm.actor_rot(a);
            let yaw = rot[1] as f32 * std::f32::consts::TAU / 65536.0;
            let eye = loc + Vec3::Y * (vm.get_prop(a, "BaseEyeHeight").map(|v| v.as_float()).unwrap_or(30.0) / UU_PER_M);
            let fwd = Vec3::new(yaw.cos(), 0.0, yaw.sin());
            let peri = vm.get_prop(a, "PeripheralVision").map(|v| v.as_float()).unwrap_or(0.0).clamp(-1.0, 1.0);
            let half = peri.acos();
            let range = (vm.get_prop(a, "SightRadius").map(|v| v.as_float()).unwrap_or(2000.0) / UU_PER_M).min(25.0);
            g.arrow(eye, eye + fwd * 1.5, Color::srgb(1.0, 0.9, 0.2));
            for s in [-half, half] {
                let d = Quat::from_rotation_y(-s) * fwd;
                g.line(eye, eye + d * range, Color::srgba(1.0, 0.9, 0.2, 0.25));
            }
            if let Some(ctrl) = obj(vm, a, "Controller") {
                if let Some(t) = obj(vm, ctrl, "Enemy").or_else(|| obj(vm, ctrl, "Focus")) {
                    let tl = uu(vm.actor_loc(t));
                    g.line(eye, tl, Color::srgb(1.0, 0.15, 0.15));
                }
                if let Some(mt) = obj(vm, ctrl, "MoveTarget") {
                    g.line(loc, uu(vm.actor_loc(mt)), Color::srgba(0.3, 1.0, 0.3, 0.7));
                }
            }
        }
    }
    // Selection.
    if let Some(s) = st.selected.filter(|&s| vm.objects.get(s).is_some_and(|o| !o.deleted)) {
        let (loc, r, h, len) = gizmo_geom(vm, s);
        g.cube(Transform::from_translation(loc).with_scale(Vec3::new(r, h, r) * 2.0), Color::srgb(1.0, 0.75, 0.1));
        let hot = st.drag.and_then(|d| d.axis);
        let col = |i: usize, c: Color| if hot == Some(i) { Color::WHITE } else { c };
        match st.gizmo_mode {
            GizmoMode::Move => {
                g.arrow(loc, loc + Vec3::X * len, col(0, Color::srgb(1.0, 0.25, 0.25)));
                g.arrow(loc, loc + Vec3::Z * len, col(2, Color::srgb(0.25, 1.0, 0.25)));
                g.arrow(loc, loc + Vec3::Y * len, col(1, Color::srgb(0.35, 0.55, 1.0)));
            }
            GizmoMode::Rotate => {
                let c = if st.drag.is_some() { Color::WHITE } else { Color::srgb(0.35, 0.55, 1.0) };
                g.circle(Isometry3d::new(loc, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)), len, c).resolution(48);
                let yaw = vm.actor_rot(s)[1] as f32 * std::f32::consts::TAU / 65536.0;
                g.arrow(loc, loc + Vec3::new(yaw.cos(), 0.0, yaw.sin()) * len, Color::srgb(1.0, 0.85, 0.2));
            }
        }
    }
}

/// Selection gizmo placement (Bevy space): centre, half extents and handle length.
fn gizmo_geom(vm: &Vm, s: ObjId) -> (Vec3, f32, f32, f32) {
    let loc = uu(vm.actor_loc(s));
    let r = (vm.get_prop(s, "CollisionRadius").map(|v| v.as_float()).unwrap_or(16.0) / UU_PER_M).max(0.3);
    let h = (vm.get_prop(s, "CollisionHeight").map(|v| v.as_float()).unwrap_or(16.0) / UU_PER_M).max(0.3);
    (loc, r, h, r.max(h) + 0.8)
}

/// Bevy-space point back to Unreal units.
fn to_uu(p: Vec3) -> [f32; 3] {
    [p.x * UU_PER_M, p.z * UU_PER_M, p.y * UU_PER_M]
}

/// Parameter along the line `o + a*t` closest to the ray `ro + rd*s`.
fn closest_on_axis(o: Vec3, a: Vec3, ro: Vec3, rd: Vec3) -> Option<f32> {
    let w = o - ro;
    let (b, d, e) = (a.dot(rd), a.dot(w), rd.dot(w));
    let denom = 1.0 - b * b;
    if denom.abs() < 1e-4 {
        return None;
    }
    Some((b * e - d) / denom)
}

/// Screen distance from point `p` to segment `a`-`b`.
fn seg_dist(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

const AXES: [Vec3; 3] = [Vec3::X, Vec3::Y, Vec3::Z];

fn cam_forward(cams: &Query<&GlobalTransform, With<FlyCam>>) -> Vec3 {
    cams.single().map(|t| t.forward().as_vec3()).unwrap_or(Vec3::NEG_Z)
}

/// Height (Bevy Y) for the reference grid: the player's feet, else just below the camera.
fn floor_y(vm: &Vm, pawn: Option<ObjId>, cam: Vec3) -> f32 {
    match pawn {
        Some(p) => {
            let l = uu(vm.actor_loc(p));
            l.y - vm.get_prop(p, "CollisionHeight").map(|v| v.as_float()).unwrap_or(72.0) / UU_PER_M
        }
        None => cam.y - 1.5,
    }
}

// ---------------------------------------------------------------------------------------------
// Actor labels (screen-space text)
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct LabelPool(Vec<Entity>);

fn draw_labels(
    mut commands: Commands,
    st: Res<DevState>,
    sim: NonSend<Sim>,
    cams: Query<(&Camera, &GlobalTransform), With<FlyCam>>,
    mut pool: Local<LabelPool>,
    mut labels: Query<(&mut Node, &mut Text, &mut Visibility), With<ActorLabel>>,
) {
    let want = st.ov.labels || st.editor;
    let Ok((cam, ctf)) = cams.single() else { return };
    let vm = &sim.game.world.vm;
    let mut items: Vec<(Vec2, String, f32)> = Vec::new();
    if want {
        let cp = ctf.translation();
        let player_pawn = sim.game.player_controller.and_then(|pc| obj(vm, pc, "Pawn"));
        for &a in &vm.actors {
            if vm.objects[a].deleted || Some(a) == player_pawn || !pickable(vm, a) {
                continue;
            }
            let l = uu(vm.actor_loc(a));
            let d = l.distance(cp);
            if d > 40.0 {
                continue;
            }
            let h = vm.get_prop(a, "CollisionHeight").map(|v| v.as_float()).unwrap_or(0.0) / UU_PER_M;
            let Ok(p) = cam.world_to_viewport(ctf, l + Vec3::Y * (h + 0.25)) else { continue };
            let mut s = vm.object_name(a);
            if vm.class_is_a(vm.objects[a].class, "Pawn") {
                let hp = vm.get_prop(a, "Health").map(|v| v.as_float()).unwrap_or(0.0);
                let ctrl = obj(vm, a, "Controller").map(|c| vm.state_name(c)).unwrap_or_default();
                s = format!("{s}  {hp:.0}hp\n{ctrl}");
            }
            items.push((p, s, d));
        }
        items.sort_by(|a, b| a.2.total_cmp(&b.2));
        items.truncate(48);
    }
    while pool.0.len() < items.len() {
        let e = commands
            .spawn((
                ActorLabel,
                Text::new(""),
                TextFont { font_size: bevy::text::FontSize::Px(11.0), ..default() },
                TextColor(Color::srgb(0.95, 0.95, 0.85)),
                TextShadow::default(),
                Node { position_type: PositionType::Absolute, ..default() },
                GlobalZIndex(i32::MAX - 40),
                Visibility::Hidden,
            ))
            .id();
        pool.0.push(e);
        return; // spawned this frame; filled next frame
    }
    for (i, &e) in pool.0.iter().enumerate() {
        let Ok((mut n, mut t, mut v)) = labels.get_mut(e) else { continue };
        match items.get(i) {
            Some((p, s, _)) => {
                n.left = px(p.x);
                n.top = px(p.y);
                if t.0 != *s {
                    t.0 = s.clone();
                }
                *v = Visibility::Inherited;
            }
            None => *v = Visibility::Hidden,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Panel refresh
// ---------------------------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn refresh_panel(
    mut commands: Commands,
    time: Res<Time>,
    mut st: ResMut<DevState>,
    overlay: Res<crate::debug::DebugOverlay>,
    sim: NonSend<Sim>,
    mut root: Query<(Entity, &mut Visibility, &mut Node, &mut BackgroundColor), With<DevRoot>>,
    mut bar: Query<&mut Visibility, (With<EditorBar>, Without<DevRoot>)>,
    mut bar_text: Query<&mut Text, With<EditorBarText>>,
    mut toast: Query<&mut Text, (With<DevToast>, Without<EditorBarText>)>,
) {
    let Ok((root_e, mut vis, mut node, mut root_bg)) = root.single_mut() else { return };
    let collapsed = st.editor && st.panel_hidden;
    let w = if collapsed { Val::Auto } else { px(430.0) };
    if node.width != w {
        node.width = w;
        root_bg.0 = if collapsed { Color::NONE } else { BG };
    }
    let show = st.open || st.editor;
    let want = if show { Visibility::Inherited } else { Visibility::Hidden };
    if *vis != want {
        *vis = want;
        st.rebuild = true;
    }
    if let Ok(mut b) = bar.single_mut() {
        let w = if st.editor { Visibility::Inherited } else { Visibility::Hidden };
        if *b != w {
            *b = w;
        }
    }
    let vm = &sim.game.world.vm;
    // Inspector text for the selection (refreshed a few times a second).
    st.since_refresh += time.delta_secs();
    if st.since_refresh > 0.33 {
        st.since_refresh = 0.0;
        let ins = st.selected.filter(|&s| vm.objects.get(s).is_some_and(|o| !o.deleted)).map(|s| inspect(vm, s)).unwrap_or_default();
        if ins != st.inspector {
            st.inspector = ins;
            if st.tab == Tab::Editor {
                st.rebuild = true;
            }
        }
        let stats = format!(
            "{}  |  actors {}  |  errors {}  |  speed {}x{}",
            sim.game.host.map_name.borrow(),
            sim.game.live_actors(),
            vm.script_errors.values().sum::<u64>(),
            st.game_speed,
            if sim.paused { "  |  PAUSED" } else { "" }
        );
        if stats != st.stats {
            st.stats = stats;
            if show {
                // Footer text only: cheap to rebuild with the panel.
                st.rebuild = true;
            }
        }
        if let Ok(mut t) = bar_text.single_mut() {
            let sel = st.selected.filter(|&s| vm.objects.get(s).is_some_and(|o| !o.deleted)).map(|s| {
                let l = vm.actor_loc(s);
                format!("{} [{}]  {:.0} {:.0} {:.0}", vm.object_name(s), vm.classes[vm.objects[s].class].name, l[0], l[1], l[2])
            });
            t.0 = format!(
                "EDITOR  {}   |   mode {:?}{}   |   {}   |   H panel   F11 exit",
                sel.unwrap_or_else(|| "no selection (LMB)".into()),
                st.gizmo_mode,
                if st.snap { " snap" } else { "" },
                "RMB look  WASD/QE fly  arrows nudge  End drop  Del delete  Ctrl+D dup  F go to"
            );
        }
    }
    if let Ok(mut t) = toast.single_mut() {
        let s = st.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
        if t.0 != s {
            t.0 = s;
        }
    }
    if st.rebuild && show {
        st.rebuild = false;
        if std::env::var_os("P2GAME_DEV_TRACE").is_some() {
            println!("devmenu: build panel tab {:?}", st.tab);
        }
        build_panel(&mut commands, root_e, &st, overlay.visible);
    }
}

/// Inspector text: class chain, state, pose, physics and the interesting properties.
fn inspect(vm: &Vm, a: ObjId) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let c = vm.objects[a].class;
    let mut chain = Vec::new();
    let mut k = Some(c);
    while let Some(x) = k {
        chain.push(vm.classes[x].name.clone());
        k = vm.classes[x].super_;
        if chain.len() >= 5 {
            break;
        }
    }
    let _ = writeln!(s, "{}", vm.object_name(a));
    let _ = writeln!(s, "class {}", chain.join(" < "));
    let st = vm.state_name(a);
    if !st.is_empty() {
        let _ = writeln!(s, "state {st}");
    }
    let l = vm.actor_loc(a);
    let r = vm.actor_rot(a);
    let _ = writeln!(s, "location {:.0} {:.0} {:.0}", l[0], l[1], l[2]);
    let _ = writeln!(s, "rotation p{} y{} r{}", r[0] & 0xffff, r[1] & 0xffff, r[2] & 0xffff);
    const PHYS: [&str; 15] = ["None", "Walking", "Falling", "Swimming", "Flying", "Rotating", "Projectile", "Interpolating", "MovingBrush", "Spider", "Trailer", "Ladder", "RootMotion", "Karma", "KarmaRagDoll"];
    let ph = vm.get_prop(a, "Physics").map(|v| v.as_int()).unwrap_or(0) as usize;
    let v = vm.get_prop(a, "Velocity").map(|v| v.as_vec()).unwrap_or_default();
    let _ = writeln!(s, "physics {}  velocity {:.0} {:.0} {:.0}", PHYS.get(ph).copied().unwrap_or("?"), v[0], v[1], v[2]);
    let short = |v: &Value| -> String {
        let t = match v {
            Value::Object(Some(o)) => vm.object_name(*o),
            Value::Object(None) => "None".into(),
            Value::Float(f) => format!("{f:.2}"),
            Value::Vector(x) => format!("{:.0} {:.0} {:.0}", x[0], x[1], x[2]),
            Value::Resource(p) => p.rsplit('.').next().unwrap_or(p).to_string(),
            Value::Class(Some(c)) => vm.classes[*c].name.clone(),
            other => format!("{other:?}"),
        };
        t.chars().take(48).collect()
    };
    for k in [
        "Health", "Tag", "Event", "DrawType", "StaticMesh", "Mesh", "DrawScale", "CollisionRadius", "CollisionHeight", "bHidden", "bCollideActors", "bBlockActors", "Mass", "Controller", "Enemy", "Weapon", "Owner", "Base", "InventoryType", "AmmoAmount",
    ] {
        if let Some(v) = vm.get_prop(a, k) {
            if matches!(v, Value::Object(None) | Value::Name(_)) && matches!(k, "Owner" | "Base" | "Enemy" | "Event") {
                if let Value::Name(n) = v {
                    if n.is_empty() || n.eq_ignore_ascii_case("None") {
                        continue;
                    }
                } else {
                    continue;
                }
            }
            let _ = writeln!(s, "{k} = {}", short(v));
        }
    }
    if let Some(ctrl) = obj(vm, a, "Controller") {
        let _ = writeln!(s, "controller state {}", vm.state_name(ctrl));
    }
    s.trim_end().to_string()
}
