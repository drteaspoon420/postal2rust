//! Keyboard loadout panel, shared by both wizard camera modes.
use super::*;
use p2magicka::catalog::Catalog;

const COLOURS: &[(&str, [f32; 3])] = &[
    ("White", [1., 1., 1.]),
    ("Red", [0.85, 0.06, 0.04]),
    ("Blue", [0.04, 0.18, 0.9]),
    ("Green", [0.08, 0.6, 0.12]),
    ("Yellow", [1., 0.8, 0.04]),
    ("Purple", [0.5, 0.08, 0.8]),
    ("Cyan", [0.04, 0.8, 0.85]),
    ("Orange", [1., 0.3, 0.03]),
    ("Pink", [1., 0.25, 0.6]),
    ("Black", [0.03, 0.03, 0.03]),
    ("Grey", [0.35, 0.35, 0.35]),
    ("Brown", [0.3, 0.12, 0.04]),
];

#[derive(Default)]
pub(super) struct Wardrobe {
    pub open: bool,
    catalog: Option<Catalog>,
    row: usize,
    indices: [usize; 4],
    message: String,
}

impl Wardrobe {
    pub fn update(&mut self, wizard: &mut WizardWorld, keys: &ButtonInput<KeyCode>) {
        if keys.just_pressed(KeyCode::F7) {
            self.open = !self.open;
            if self.open {
                if self.catalog.is_none() {
                    match Catalog::load(&wizard.content) {
                        Ok(catalog) => {
                            println!(
                                "MAGICKA CATALOG {} robes {} staffs {} melee weapons",
                                catalog.robes.len(),
                                catalog.staffs.len(),
                                catalog.weapons.len()
                            );
                            self.catalog = Some(catalog);
                        }
                        Err(error) => self.message = error,
                    }
                }
                if let Some(catalog) = &self.catalog {
                    let gear = wizard.equipped_loadout();
                    for (row, (list, selected)) in [
                        (&catalog.robes, Some(gear.robe.as_str())),
                        (&catalog.staffs, gear.staff.as_deref()),
                        (&catalog.weapons, gear.weapon.as_deref()),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        self.indices[row] = list
                            .iter()
                            .position(|name| selected.is_some_and(|s| s.eq_ignore_ascii_case(name)))
                            .unwrap_or(0);
                    }
                    self.message = "Choose your loadout, then press Enter to equip.".into();
                    self.indices[3] = COLOURS
                        .iter()
                        .position(|(_, rgb)| *rgb == wizard.robe_colour)
                        .unwrap_or(0);
                }
            }
        }
        if !self.open {
            return;
        }
        if keys.just_pressed(KeyCode::Escape) {
            self.open = false;
            return;
        }
        if keys.just_pressed(KeyCode::ArrowUp) {
            self.row = (self.row + 3) % 4;
        }
        if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::Tab) {
            self.row = (self.row + 1) % 4;
        }
        let Some(catalog) = &self.catalog else { return };
        let count = if self.row == 3 {
            COLOURS.len()
        } else {
            [&catalog.robes, &catalog.staffs, &catalog.weapons][self.row].len()
        };
        if count > 0 {
            let step = if keys.just_pressed(KeyCode::ArrowRight) {
                1
            } else if keys.just_pressed(KeyCode::ArrowLeft) {
                -1
            } else if keys.just_pressed(KeyCode::PageDown) {
                10
            } else if keys.just_pressed(KeyCode::PageUp) {
                -10
            } else {
                0
            };
            self.indices[self.row] =
                (self.indices[self.row] as i32 + step).rem_euclid(count as i32) as usize;
            if keys.just_pressed(KeyCode::Home) {
                self.indices[self.row] = 0;
            }
            if keys.just_pressed(KeyCode::End) {
                self.indices[self.row] = count - 1;
            }
        }
        if keys.just_pressed(KeyCode::Enter) {
            let (Some(robe), Some(staff), Some(weapon)) = (
                catalog.robes.get(self.indices[0]),
                catalog.staffs.get(self.indices[1]),
                catalog.weapons.get(self.indices[2]),
            ) else {
                return;
            };
            match wizard.equip(Loadout {
                robe: robe.clone(),
                staff: Some(staff.clone()),
                weapon: Some(weapon.clone()),
            }) {
                Ok(()) => {
                    wizard.robe_colour = COLOURS[self.indices[3]].1;
                    self.message = "Equipped. F7 or Esc returns to the mayhem.".into();
                    println!("MAGICKA EQUIPPED {:?}", wizard.equipped_loadout());
                    println!(
                        "MAGICKA COLOUR {} {:?}",
                        COLOURS[self.indices[3]].0, wizard.robe_colour
                    );
                }
                Err(error) => self.message = format!("Could not equip: {error}"),
            }
        }
    }

    fn text(&self) -> String {
        let mut text = String::from("WIZARD LOADOUT\n\n");
        if let Some(catalog) = &self.catalog {
            for (row, (label, list)) in [
                ("Robe", &catalog.robes),
                ("Staff", &catalog.staffs),
                ("Melee", &catalog.weapons),
            ]
            .into_iter()
            .enumerate()
            {
                let selected = list
                    .get(self.indices[row])
                    .map(String::as_str)
                    .unwrap_or("Unavailable");
                text.push_str(&format!(
                    "{} {}  ({}/{})\n   {}\n",
                    if self.row == row { ">" } else { " " },
                    label,
                    if list.is_empty() {
                        0
                    } else {
                        self.indices[row] + 1
                    },
                    list.len(),
                    selected.replace('_', " ")
                ));
            }
        }
        let (name, _) = COLOURS[self.indices[3]];
        text.push_str(&format!(
            "{} Colour  ({}/{})\n   {}\n",
            if self.row == 3 { ">" } else { " " },
            self.indices[3] + 1,
            COLOURS.len(),
            name
        ));
        text.push_str("\nUp/Down category · Left/Right choose\nPgUp/PgDn skip 10 · Home/End first/last\nEnter equip · F6 camera · F7/Esc close\n\n");
        text.push_str(&self.message);
        text.push_str("\n\nThe world keeps running while you choose.");
        text
    }
}

#[derive(Component)]
pub(super) struct WardrobeText;
pub(super) fn setup(mut commands: Commands) {
    commands.spawn((
        WardrobeText,
        Text::new(""),
        TextFont {
            font_size: bevy::text::FontSize::Px(16.),
            ..default()
        },
        TextColor(Color::srgb(0.95, 0.85, 0.55)),
        BackgroundColor(Color::srgba(0.025, 0.015, 0.05, 0.96)),
        Node {
            position_type: PositionType::Absolute,
            left: px(20.),
            top: px(20.),
            width: px(500.),
            max_width: percent(92.),
            padding: UiRect::all(px(16.)),
            ..default()
        },
        Visibility::Hidden,
        GlobalZIndex(i32::MAX),
    ));
}
pub(super) fn draw(
    state: NonSend<MagicState>,
    ui: Res<UiInput>,
    mut panel: Query<(&mut Text, &mut Visibility), With<WardrobeText>>,
    mut hud: Query<&mut Visibility, (With<MagicHud>, Without<WardrobeText>)>,
) {
    for mut visibility in &mut hud {
        *visibility = if state.wardrobe.open || !ui.player_view {
            Visibility::Hidden
        } else {
            Visibility::Visible
        };
    }
    for (mut text, mut visibility) in &mut panel {
        *visibility = if state.wardrobe.open && ui.game_look && !ui.captured {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *visibility == Visibility::Visible {
            **text = state.wardrobe.text();
        }
    }
}
