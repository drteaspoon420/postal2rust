//! Compact wizard HUD over Postal's own: the queued elements and the element
//! keys drawn with the original `UI/HUD/hud.xnb` icons, leaving the view free.
use super::MagicState;
use crate::ui::UiInput;
use bevy::prelude::*;

/// Element icon cell in hud.xnb (KeyboardHUD.DrawIcon: 50 px cells from
/// v 156, five per row, in `Elements::index` order).
fn icon_rect(index: usize) -> Rect {
    let u = (index % 5) as f32 * 50.;
    let v = 156. + (index / 5) as f32 * 50.;
    Rect::new(u, v, u + 50., v + 50.)
}
/// Element indices of the eight keys (1..8 / Q W E R A S D F): Water, Life,
/// Shield, Cold, Lightning, Arcane, Earth, Fire.
const KEY_ELEMENTS: [usize; 8] = [1, 6, 7, 2, 4, 5, 0, 3];
const QUEUE: usize = 5;

#[derive(Component)]
pub(super) struct QueueIcon(usize);
#[derive(Component)]
pub(super) struct KeyLabel(usize);
#[derive(Component)]
pub(super) struct WizardHudRoot;

#[derive(Default)]
pub(super) struct Atlas {
    loaded: bool,
}

/// Build the icon rows once the wizard (and so its content directory) exists.
#[allow(clippy::too_many_arguments)]
pub(super) fn icons(
    mut commands: Commands,
    state: NonSend<MagicState>,
    ui: Res<UiInput>,
    mut atlas: Local<Atlas>,
    mut images: ResMut<Assets<Image>>,
    mut queue: Query<(&QueueIcon, &mut ImageNode, &mut Node), Without<KeyLabel>>,
    mut labels: Query<(&KeyLabel, &mut Text)>,
    mut roots: Query<&mut Visibility, With<WizardHudRoot>>,
) {
    let Some(wizard) = &state.world else { return };
    if !atlas.loaded {
        atlas.loaded = true;
        let texture = match p2magicka::visual::texture2d(&wizard.content, "UI/HUD/hud") {
            Ok(t) => t,
            Err(error) => {
                eprintln!("MAGICKA hud: {error}");
                return;
            }
        };
        let image = images.add(Image::new(
            bevy::render::render_resource::Extent3d {
                width: texture.width,
                height: texture.height,
                depth_or_array_layers: 1,
            },
            bevy::render::render_resource::TextureDimension::D2,
            texture.rgba,
            bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
            bevy::asset::RenderAssetUsages::default(),
        ));
        // Spell queue: bottom centre, like Magicka's conjuring orbs.
        commands
            .spawn((
                WizardHudRoot,
                Node {
                    position_type: PositionType::Absolute,
                    bottom: px(18.),
                    width: percent(100.),
                    justify_content: JustifyContent::Center,
                    column_gap: px(4.),
                    ..default()
                },
                GlobalZIndex(i32::MAX - 3),
            ))
            .with_children(|row| {
                for i in 0..QUEUE {
                    row.spawn((
                        QueueIcon(i),
                        ImageNode::new(image.clone()).with_rect(icon_rect(0)),
                        Node {
                            width: px(44.),
                            height: px(44.),
                            display: Display::None,
                            ..default()
                        },
                    ));
                }
            });
        // Element keys: bottom left.
        commands
            .spawn((
                WizardHudRoot,
                Node {
                    position_type: PositionType::Absolute,
                    bottom: px(12.),
                    left: px(12.),
                    column_gap: px(2.),
                    ..default()
                },
                GlobalZIndex(i32::MAX - 3),
            ))
            .with_children(|row| {
                for (k, element) in KEY_ELEMENTS.into_iter().enumerate() {
                    row.spawn(Node {
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        ..default()
                    })
                    .with_children(|key| {
                        key.spawn((
                            ImageNode::new(image.clone()).with_rect(icon_rect(element)),
                            Node {
                                width: px(26.),
                                height: px(26.),
                                ..default()
                            },
                        ));
                        key.spawn((
                            KeyLabel(k),
                            Text::new(""),
                            TextFont {
                                font_size: bevy::text::FontSize::Px(12.),
                                ..default()
                            },
                            TextColor(Color::srgb(0.95, 0.85, 0.55)),
                            TextShadow::default(),
                        ));
                    });
                }
            });
        return;
    }
    let visible = ui.player_view && !state.wardrobe.open;
    for mut v in &mut roots {
        *v = if visible { Visibility::Visible } else { Visibility::Hidden };
    }
    let elements = &wizard.avatar().conjure.queue;
    for (icon, mut image, mut node) in &mut queue {
        match elements.get(icon.0).and_then(|s| s.mag.iter().position(|m| *m > 0.)) {
            Some(index) => {
                image.rect = Some(icon_rect(index));
                node.display = Display::Flex;
            }
            None => node.display = Display::None,
        }
    }
    for (label, mut text) in &mut labels {
        let key = if state.classic {
            ["Q", "W", "E", "R", "A", "S", "D", "F"][label.0].to_string()
        } else {
            (label.0 + 1).to_string()
        };
        if text.0 != key {
            text.0 = key;
        }
    }
}
