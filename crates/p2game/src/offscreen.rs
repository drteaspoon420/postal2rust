//! Offscreen QA runs the normal game, input and renderer without a display
//! server. A Window component supplies viewport dimensions to the existing UI;
//! every window camera renders into the same image instead of an OS surface.
use bevy::{
    camera::{CameraUpdateSystems, RenderTarget},
    prelude::*,
    render::view::screenshot::Screenshot,
};
use std::{sync::Mutex, time::Duration};
// Startup can run on a Bevy worker, while screenshot requests run on the main
// thread with the VM. Both must see the same image target.
static CAPTURE: Mutex<Option<Handle<Image>>> = Mutex::new(None);
#[derive(Resource)]
struct Output(Handle<Image>);

pub(crate) struct OffscreenPlugin {
    pub window: Window,
}
impl Plugin for OffscreenPlugin {
    fn build(&self, app: &mut App) {
        let mut window = self.window.clone();
        window.resolution.set_physical_resolution(800, 450);
        app.world_mut().spawn((window, bevy::window::PrimaryWindow));
        app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::from_secs_f64(1. / 60.),
        ))
        .add_plugins(bevy::app::ScheduleRunnerPlugin::run_loop(Duration::ZERO))
        .add_systems(Startup, setup)
        .add_systems(PostUpdate, retarget.before(CameraUpdateSystems));
    }
}
fn setup(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let mut image = Image::new_target_texture(
        800,
        450,
        bevy::render::render_resource::TextureFormat::Bgra8UnormSrgb,
        None,
    );
    image.texture_descriptor.usage |= bevy::render::render_resource::TextureUsages::COPY_SRC;
    let image = images.add(image);
    *CAPTURE.lock().unwrap() = Some(image.clone());
    commands.insert_resource(Output(image));
    println!("OFFSCREEN: normal game renderer targeting 800x450 image");
}
fn retarget(output: Res<Output>, mut targets: Query<&mut RenderTarget, With<Camera>>) {
    for mut target in &mut targets {
        if matches!(*target, RenderTarget::Window(_)) {
            *target = RenderTarget::Image(output.0.clone().into());
        }
    }
}
pub(crate) fn screenshot() -> Screenshot {
    CAPTURE
        .lock()
        .unwrap()
        .as_ref()
        .map_or_else(Screenshot::primary_window, |h| Screenshot::image(h.clone()))
}
