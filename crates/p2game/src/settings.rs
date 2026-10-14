//! Video settings that the original shell menus (MenuVideo, MenuPerformanceAdvanced) drive,
//! mapped onto our Bevy renderer.
//!
//! The menus talk to the engine through `ConsoleCommand`: `GetCurrentRes`, `GetFullScreen`,
//! `GETSUPPORTEDRESOLUTION INDEX=n`, `SetRes WxHxW|F`, `SetFullscreenWindowed`, `Brightness x`,
//! `Contrast x`, `Gamma x`, plus `get/set ini:Engine.Engine.ViewportManager <Key>` (the ini keys
//! are saved by the VM's GET/SET into the user Postal2.ini, section `[WinDrv.WindowsClient]`).
//! This extension host answers them from a shared [`Video`] state, which [`apply`] pushes to the
//! window (resolution, windowed / exclusive fullscreen / borderless, vsync), a display grade
//! pass (brightness/contrast/gamma) and a frame rate limiter. The window starts from the saved
//! user ini values, so a choice made in the menu also holds on the next start.
//!
//! Controls with no possible effect here are hidden when their menu is built (OpenGL renderer,
//! texture detail sliders, anisotropy, detail textures) and the remaining items move up. The
//! Triple Buffering checkbox becomes the VSync checkbox (MenuVideo's own VSync control is
//! commented out in the shipped script). Brightness/contrast/gamma also work in windowed mode
//! (UE2 greys them out there because it uses the desktop gamma ramp).
//!
//! Extra commands (console, `P2GAME_EXEC`): `FPSLimit N` (0 = off) and `VSync 0|1`, saved under
//! `[postal2rust.Video]` in the user Postal2.ini.
use bevy::asset::{AssetPath, embedded_asset, embedded_path};
use bevy::core_pipeline::fullscreen_material::{FullscreenMaterial, FullscreenMaterialPlugin};
use bevy::core_pipeline::{Core2d, Core2dSystems};
use bevy::ecs::schedule::{IntoScheduleConfigs, ScheduleConfigs, ScheduleLabel};
use bevy::ecs::system::BoxedSystem;
use bevy::prelude::*;
use bevy::render::extract_component::ExtractComponent;
use bevy::render::render_resource::ShaderType;
use bevy::shader::ShaderRef;
use bevy::window::{MonitorSelection, PresentMode, PrimaryWindow, VideoModeSelection, WindowMode};
use std::rc::Rc;
use std::sync::Mutex;
use uscript::vm::{ObjId, Value, Vm};

const SECTION: &str = "WinDrv.WindowsClient";
const OURS: &str = "postal2rust.Video";
const INI: &str = "Postal2.ini";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Windowed,
    Fullscreen,
    Borderless,
}

#[derive(Clone, Debug)]
pub(crate) struct Video {
    pub width: u32,
    pub height: u32,
    pub mode: Mode,
    pub vsync: bool,
    pub fps_limit: u32,
    pub brightness: f32,
    pub contrast: f32,
    pub gamma: f32,
    /// Window settings changed since the last [`apply`].
    dirty: bool,
}

impl Default for Video {
    fn default() -> Self {
        Self { width: 1600, height: 900, mode: Mode::Windowed, vsync: true, fps_limit: 0, brightness: 0.5, contrast: 0.5, gamma: 1.0, dirty: false }
    }
}

// Global, not thread_local: the VM (main thread) writes it, Bevy systems on worker threads read it.
static VIDEO: Mutex<Option<Video>> = Mutex::new(None);
/// Monitor video modes (w, h, refresh Hz), largest last.
static MODES: Mutex<Vec<(u32, u32, u32)>> = Mutex::new(Vec::new());

fn with_video<R>(f: impl FnOnce(&mut Video) -> R) -> R {
    let mut g = VIDEO.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Video::default))
}

pub(crate) fn video() -> Video {
    with_video(|v| v.clone())
}

fn edit(f: impl FnOnce(&mut Video)) {
    with_video(|v| {
        f(v);
        v.dirty = true;
    });
}

fn truthy(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
}

fn user_value(vm: &mut Vm, section: &str, key: &str) -> Option<String> {
    vm.user_ini_section(INI, section).into_iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v)
}

fn ini_float(vm: &mut Vm, key: &str) -> Option<f32> {
    vm.console_get_set(&format!("get ini:Engine.Engine.ViewportManager {key}")).and_then(|s| s.trim().parse().ok())
}

/// Scripted test runs keep the default 1600x900 window whatever the user saved (unless they
/// point `P2_USER_DIR` at their own settings).
fn automated() -> bool {
    if std::env::var_os("P2_USER_DIR").is_some() {
        return false;
    }
    let a: Vec<String> = std::env::args().collect();
    a.iter().any(|x| x == "--smoke" || x == "--progress" || x == "--tour")
        || ["P2GAME_EXEC", "P2GAME_PILOT", "P2GAME_REAL", "P2GAME_SHOTS", "P2GAME_EXIT", "P2GAME_DEMO_GOTO"].iter().any(|v| std::env::var_os(v).is_some())
}

/// Startup state from the saved user ini, then the env overrides (`P2GAME_RES=WxH`,
/// `P2GAME_FULLSCREEN=1` only when the user has not chosen a window mode, `P2GAME_NOVSYNC=1`,
/// `P2GAME_FPS_LIMIT=N`). Returns the primary window description.
pub(crate) fn initial_window(vm: &mut Vm, title: String) -> Window {
    let mut v = Video::default();
    let fs = user_value(vm, SECTION, "StartupFullscreen").map(|s| truthy(&s));
    let bl = user_value(vm, SECTION, "FullscreenWindowed").map(|s| truthy(&s));
    let auto = automated();
    // `P2GAME_WINDOWED=1` (the launcher's --windowed) wins over the saved mode for this run.
    let force_windowed = std::env::var_os("P2GAME_WINDOWED").is_some();
    v.mode = match (fs, bl) {
        _ if auto || force_windowed => Mode::Windowed,
        (_, Some(true)) => Mode::Borderless,
        (Some(true), _) => Mode::Fullscreen,
        (Some(false), _) | (_, Some(false)) => Mode::Windowed,
        _ if std::env::var_os("P2GAME_FULLSCREEN").is_some() => Mode::Borderless,
        _ => Mode::Windowed,
    };
    let key = if v.mode == Mode::Fullscreen { "Fullscreen" } else { "Windowed" };
    let num = |vm: &mut Vm, k: &str| user_value(vm, SECTION, k).and_then(|s| s.trim().parse::<u32>().ok()).filter(|n| *n >= 320);
    if !auto {
        if let (Some(w), Some(h)) = (num(vm, &format!("{key}ViewportX")), num(vm, &format!("{key}ViewportY"))) {
            (v.width, v.height) = (w, h);
        }
    }
    if let Some((w, h)) = std::env::var("P2GAME_RES").ok().and_then(|s| s.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))) {
        (v.width, v.height) = (w, h);
    }
    v.vsync = user_value(vm, OURS, "VSync").map(|s| truthy(&s)).unwrap_or(true) && std::env::var_os("P2GAME_NOVSYNC").is_none();
    v.fps_limit = std::env::var("P2GAME_FPS_LIMIT").ok().or_else(|| user_value(vm, OURS, "FrameRateLimit")).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    v.brightness = ini_float(vm, "Brightness").unwrap_or(0.5).clamp(0.0, 1.0);
    v.contrast = ini_float(vm, "Contrast").unwrap_or(0.5).clamp(0.0, 2.0);
    v.gamma = ini_float(vm, "Gamma").unwrap_or(1.0).clamp(0.3, 3.0);
    println!("video: {}x{} {:?} vsync {} fps limit {} brightness {} contrast {} gamma {}", v.width, v.height, v.mode, v.vsync, v.fps_limit, v.brightness, v.contrast, v.gamma);
    let mut w = Window { title, resolution: (v.width, v.height).into(), present_mode: present(v.vsync), ..default() };
    // No window exists yet, so `MonitorSelection::Current` has nothing to resolve against and
    // winit panics ("Unable to get monitor"): open on the primary monitor.
    w.mode = match v.mode {
        Mode::Windowed => WindowMode::Windowed,
        Mode::Borderless => WindowMode::BorderlessFullscreen(MonitorSelection::Primary),
        // The exact video mode is picked in `apply` once the monitors are known.
        Mode::Fullscreen => WindowMode::Fullscreen(MonitorSelection::Primary, VideoModeSelection::Current),
    };
    v.dirty = v.mode == Mode::Fullscreen;
    with_video(|s| *s = v);
    w
}

fn present(vsync: bool) -> PresentMode {
    if vsync { PresentMode::AutoVsync } else { PresentMode::AutoNoVsync }
}

/// `SetRes 1920x1080 - 16:9xF` (menu combo text plus the window flag) / `SetRes 1280x720`.
fn parse_setres(arg: &str) -> Option<(u32, u32, Option<bool>)> {
    let nums: Vec<u32> = arg.split(|c: char| !c.is_ascii_digit()).filter_map(|s| s.parse().ok()).collect();
    let (w, h) = (*nums.first()?, *nums.get(1)?);
    let t = arg.trim().to_ascii_lowercase();
    let full = if t.ends_with("xf") { Some(true) } else if t.ends_with("xw") { Some(false) } else { None };
    (w >= 320 && h >= 200).then_some((w, h, full))
}

struct SettingsExt;

impl uscript::host::Host for SettingsExt {
    fn console_command(&self, vm: &mut Vm, _from: Option<ObjId>, command: &str) -> String {
        let c = command.split_whitespace().collect::<Vec<_>>().join(" ");
        let lc = c.to_ascii_lowercase();
        let (verb, rest) = lc.split_once(' ').unwrap_or((lc.as_str(), ""));
        let (cw, ch) = p2game::viewport::size();
        let v = video();
        let set_user = |vm: &mut Vm, sec: &str, k: &str, val: String| {
            let _ = vm.user_ini_set(INI, sec, k, &val);
        };
        match verb {
            "getcurrentres" => return format!("{cw}x{ch}"),
            "getfullscreen" => return if v.mode == Mode::Fullscreen { "True".into() } else { "False".into() },
            "getcurrentcolordepth" => return "32".into(),
            "getdisplaycount" => return "1".into(),
            "getcurrentdisplaynumber" => return "0".into(),
            "getrenderdeviceogl" => return "0".into(),
            "setrenderdeviceogl" | "flush" => return "1".into(),
            "getsupportedresolution" => {
                let i: usize = rest.rsplit('=').next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
                return MODES.lock().map(|m| m.get(i).map(|(w, h, r)| format!("{w}x{h}x{r}x32")).unwrap_or_default()).unwrap_or_default();
            }
            "setres" => {
                let Some((w, h, full)) = parse_setres(rest) else { return String::new() };
                let borderless = user_value(vm, SECTION, "FullscreenWindowed").is_some_and(|s| truthy(&s));
                let full = full.unwrap_or(v.mode == Mode::Fullscreen);
                let mode = if borderless { Mode::Borderless } else if full { Mode::Fullscreen } else { Mode::Windowed };
                let key = if full { "Fullscreen" } else { "Windowed" };
                set_user(vm, SECTION, &format!("{key}ViewportX"), w.to_string());
                set_user(vm, SECTION, &format!("{key}ViewportY"), h.to_string());
                edit(|v| {
                    (v.width, v.height, v.mode) = (w, h, mode);
                });
                println!("video: SetRes {w}x{h} {mode:?}");
                return "1".into();
            }
            "setfullscreenwindowed" => {
                let borderless = user_value(vm, SECTION, "FullscreenWindowed").is_some_and(|s| truthy(&s));
                if borderless {
                    edit(|v| v.mode = Mode::Borderless);
                } else if v.mode == Mode::Borderless {
                    edit(|v| v.mode = Mode::Windowed);
                }
                return "1".into();
            }
            "togglefullscreen" => {
                let full = v.mode == Mode::Windowed;
                set_user(vm, SECTION, "StartupFullscreen", if full { "True" } else { "False" }.into());
                edit(|v| v.mode = if full { Mode::Fullscreen } else { Mode::Windowed });
                return "1".into();
            }
            "brightness" | "contrast" | "gamma" => {
                let Ok(x) = rest.trim().parse::<f32>() else { return String::new() };
                edit(|v| match verb {
                    "brightness" => v.brightness = x.clamp(0.0, 1.0),
                    "contrast" => v.contrast = x.clamp(0.0, 2.0),
                    _ => v.gamma = x.clamp(0.3, 3.0),
                });
                return "1".into();
            }
            "fpslimit" => {
                let n: u32 = rest.trim().parse().unwrap_or(0);
                set_user(vm, OURS, "FrameRateLimit", n.to_string());
                edit(|v| v.fps_limit = n);
                return "1".into();
            }
            "vsync" => {
                let on = truthy(rest);
                set_user(vm, OURS, "VSync", if on { "True" } else { "False" }.into());
                edit(|v| v.vsync = on);
                return "1".into();
            }
            _ => {}
        }
        // The Triple Buffering checkbox drives vsync.
        if let Some(r) = lc.strip_prefix("get d3ddrv.d3drenderdevice usetriplebuffering") {
            let _ = r;
            return if v.vsync { "True".into() } else { "False".into() };
        }
        if let Some(r) = lc.strip_prefix("set d3ddrv.d3drenderdevice usetriplebuffering ") {
            return self.console_command(vm, None, &format!("VSync {}", truthy(r) as u8));
        }
        if let Some(k) = lc.strip_prefix("get ini:engine.engine.viewportmanager ") {
            return match k {
                // GetResolution (FOV aspect) reads these: always the real viewport size.
                "windowedviewportx" | "fullscreenviewportx" => cw.to_string(),
                "windowedviewporty" | "fullscreenviewporty" => ch.to_string(),
                "startupfullscreen" => user_value(vm, SECTION, "StartupFullscreen").unwrap_or_else(|| if v.mode == Mode::Fullscreen { "True".into() } else { "False".into() }),
                "fullscreenwindowed" => user_value(vm, SECTION, "FullscreenWindowed").unwrap_or_else(|| if v.mode == Mode::Borderless { "True".into() } else { "False".into() }),
                _ => String::new(),
            };
        }
        String::new()
    }
}

fn prop_obj(vm: &Vm, o: ObjId, name: &str) -> Option<ObjId> {
    vm.get_prop(o, name).and_then(|v| v.as_obj()).filter(|&x| x < vm.objects.len() && !vm.objects[x].deleted)
}

fn call(vm: &mut Vm, o: ObjId, f: &str, args: Vec<Value>) {
    let c = vm.objects[o].class;
    if let Err(e) = vm.call(c, Some(o), f, args) {
        vm.record_error(&format!("settings {f}"), &e);
    }
}

/// Controls of the original menus that cannot affect our renderer (or, on the main menu, store
/// links to DLC this port does not run).
fn dead_controls(class: &str) -> &'static [&'static str] {
    match class {
        "MenuVideo" => &["OpenGLCheckbox", "DisplayNumberSlider"],
        "MenuPerformanceAdvanced" => &["TextureDetailWorldSlider", "TextureDetailSkinSlider", "TextureDetailLightmapSlider", "AnisotropyCombo", "DetailTextureCheckbox"],
        "MenuMain" | "AWMainMenu" => &["LaunchPLChoice", "ActualDLCChoice"],
        _ => &[],
    }
}

/// After `CreateMenuContents`: hide the dead controls and close the gaps they leave.
fn menu_created(vm: &mut Vm, me: Option<ObjId>, _a: &mut [Value], after: bool) {
    let (Some(me), true) = (me, after) else { return };
    let class = vm.classes[vm.objects[me].class].name.clone();
    let dead: Vec<ObjId> = dead_controls(&class).iter().filter_map(|n| prop_obj(vm, me, n)).collect();
    if class == "MenuVideo" {
        if let Some(cb) = prop_obj(vm, me, "TripleBufferCheckbox") {
            call(vm, cb, "SetText", vec![Value::Str("VSync".into())]);
            call(vm, cb, "SetHelpText", vec![Value::Str("Wait for the monitor refresh (no tearing). Off: uncapped frame rate.".into())]);
            let on = video().vsync;
            vm.set_prop(cb, "bChecked", Value::Bool(on));
        }
    }
    if dead.is_empty() {
        return;
    }
    for &d in &dead {
        call(vm, d, "HideWindow", vec![]);
        for n in dead_controls(&class) {
            if prop_obj(vm, me, n) == Some(d) {
                vm.set_prop(me, n, Value::Object(None));
            }
        }
    }
    let Some(Value::Array(items)) = vm.get_prop(me, "MenuItems").cloned() else { return };
    let field = |s: &Value, n: &str| match s {
        Value::Struct(f) => f.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.clone()),
        _ => None,
    };
    let mut y = items.first().and_then(|s| field(s, "PosY")).map(|v| v.as_float()).unwrap_or(0.0);
    let mut kept = Vec::new();
    for mut it in items {
        let w = field(&it, "Window").and_then(|v| v.as_obj());
        if w.is_some_and(|w| dead.contains(&w)) {
            continue;
        }
        let h = field(&it, "Height").map(|v| v.as_float()).unwrap_or(0.0);
        if let Value::Struct(f) = &mut it {
            for (k, v) in f.iter_mut() {
                if k.eq_ignore_ascii_case("PosY") {
                    *v = Value::Float(y);
                }
            }
        }
        if let Some(w) = w {
            vm.set_prop(w, "WinTop", Value::Float(y));
        }
        y += h;
        kept.push(it);
    }
    vm.set_prop(me, "MenuItems", Value::Array(kept));
}

/// MenuVideo greys out the gamma sliders in windowed mode (UE2 used the desktop gamma ramp);
/// our grade pass works in every mode.
fn gamma_sliders(vm: &mut Vm, me: Option<ObjId>, _a: &mut [Value], after: bool) {
    let (Some(me), true) = (me, after) else { return };
    for (s, help) in [("BrightnessSlider", "BrightnessHelp"), ("ContrastSlider", "ContrastHelp"), ("GammaSlider", "GammaHelp")] {
        if let Some(o) = prop_obj(vm, me, s) {
            vm.set_prop(o, "bActive", Value::Bool(true));
            if let Some(h) = vm.get_prop(me, help).cloned() {
                vm.set_prop(o, "HelpText", h);
            }
        }
    }
}

/// MenuMain.AddSocialIcons: the website / Facebook / Twitter buttons and the POSTAL 4 or
/// Paradise Lost store banner are closed again as soon as they are made.
fn social_icons(vm: &mut Vm, me: Option<ObjId>, _a: &mut [Value], after: bool) {
    let (Some(me), true) = (me, after) else { return };
    call(vm, me, "RemoveSocialIcons", vec![]);
    for n in ["WebsiteIcon", "FacebookIcon", "TwitterIcon", "Postal4Icon", "ParadiseLostIcon"] {
        vm.set_prop(me, n, Value::Object(None));
    }
}

/// Main-menu version line shown by this port instead of the install's "Postal2: Steam-int-5100".
pub(crate) const VERSION_TEXT: &str = "Postal2: Rust/Bevy v1";

/// FontInfo.DrawTextEx(Canvas, CanvasWidth, X, Y, Str, ...): the root window draws
/// `InstalledDescription - EngineVersion HotfixText` at the bottom left of the main menu.
fn draw_text_ex(_vm: &mut Vm, _me: Option<ObjId>, a: &mut [Value], after: bool) {
    if after {
        return;
    }
    if let Some(s @ Value::Str(_)) = a.get_mut(4) {
        if is_version_line(&s.as_str()) {
            *s = Value::Str(VERSION_TEXT.into());
        }
    }
}

fn is_version_line(s: &str) -> bool {
    // InstalledTitle ": " InstalledType "-" InstalledCountry "-" EngineVersion [HotfixText]
    let Some((title, rest)) = s.split_once(": ") else { return false };
    title.eq_ignore_ascii_case("postal2") && rest.split('-').count() >= 3 && rest.split('-').nth(2).is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
}

fn level_hook(host: &p2game::host::GameHost, vm: &mut Vm) {
    // First in the list: these answers win over the fixed viewport replies of older hosts.
    host.ext.borrow_mut().insert(0, Rc::new(SettingsExt));
    vm.watch_calls("CreateMenuContents", menu_created);
    vm.watch_calls("SetGammaSlidersEnabled", gamma_sliders);
    vm.watch_calls("AddSocialIcons", social_icons);
    vm.watch_calls("DrawTextEx", draw_text_ex);
}

pub(crate) fn install(sim: &mut crate::window::Sim) {
    p2game::host::register_level_hook(level_hook);
    level_hook(&sim.game.host, &mut sim.game.world.vm);
}

/// Display grade over the finished frame (drawn by the canvas camera, after the HUD).
#[derive(Component, ExtractComponent, Clone, Copy, ShaderType, Default)]
struct Grade {
    offset: f32,
    slope: f32,
    inv_gamma: f32,
    _pad: f32,
}

impl FullscreenMaterial for Grade {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Path(AssetPath::from_path_buf(embedded_path!("settings.wgsl")).with_source("embedded"))
    }
    fn schedule() -> impl ScheduleLabel + Clone {
        Core2d
    }
    fn schedule_configs(system: ScheduleConfigs<BoxedSystem>) -> ScheduleConfigs<BoxedSystem> {
        system.in_set(Core2dSystems::PostProcess)
    }
}

impl Grade {
    fn of(v: &Video) -> Option<Grade> {
        let g = Grade { offset: (v.brightness - 0.5) * 0.5, slope: 0.5 + v.contrast, inv_gamma: 1.0 / v.gamma.max(0.1), _pad: 0.0 };
        let identity = g.offset.abs() < 1e-3 && (g.slope - 1.0).abs() < 1e-3 && (g.inv_gamma - 1.0).abs() < 1e-3;
        (!identity).then_some(g)
    }
}

pub(crate) struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "settings.wgsl");
        app.add_plugins(FullscreenMaterialPlugin::<Grade>::default()).add_systems(Update, apply).add_systems(Last, frame_limit);
    }
}

fn apply(
    mut commands: Commands,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    monitors: Query<&bevy::window::Monitor>,
    canvas: Query<Entity, With<p2canvas::CanvasCamera>>,
    mut first: Local<bool>,
    _main_thread: bevy::ecs::system::NonSendMarker,
) {
    if MODES.lock().is_ok_and(|m| m.is_empty()) {
        let mut modes: Vec<(u32, u32, u32)> = Vec::new();
        for m in &monitors {
            for vm in &m.video_modes {
                let (w, h, r) = (vm.physical_size.x, vm.physical_size.y, vm.refresh_rate_millihertz.div_ceil(1000));
                match modes.iter_mut().find(|e| e.0 == w && e.1 == h) {
                    Some(e) => e.2 = e.2.max(r),
                    None => modes.push((w, h, r)),
                }
            }
        }
        modes.sort();
        if let Ok(mut m) = MODES.lock() {
            *m = modes;
        }
    }
    let dirty = with_video(|v| std::mem::take(&mut v.dirty));
    if !dirty && *first {
        return;
    }
    *first = true;
    let v = video();
    if let Ok(e) = canvas.single() {
        match Grade::of(&v) {
            Some(g) => {
                commands.entity(e).insert(g);
            }
            None => {
                commands.entity(e).remove::<Grade>();
            }
        }
    }
    let Ok(mut win) = windows.single_mut() else { return };
    println!("video: apply {}x{} {:?} vsync {} grade {} (window {}x{} scale {})", v.width, v.height, v.mode, v.vsync, Grade::of(&v).is_some(), win.physical_width(), win.physical_height(), win.scale_factor());
    let mode = match v.mode {
        Mode::Windowed => WindowMode::Windowed,
        Mode::Borderless => WindowMode::BorderlessFullscreen(MonitorSelection::Current),
        Mode::Fullscreen => {
            let pick = monitors.iter().flat_map(|m| m.video_modes.iter()).filter(|m| m.physical_size == UVec2::new(v.width, v.height) && m.bit_depth >= 24).max_by_key(|m| m.refresh_rate_millihertz).cloned();
            WindowMode::Fullscreen(MonitorSelection::Current, pick.map_or(VideoModeSelection::Current, VideoModeSelection::Specific))
        }
    };
    if win.mode != mode {
        win.mode = mode;
    }
    if v.mode == Mode::Windowed && (win.physical_width() != v.width || win.physical_height() != v.height) {
        // Ask the OS window to resize; winit's Resized event then updates `Window.resolution`
        // together with the surface. Setting the resolution directly resized the depth texture a
        // frame before the swapchain (wgpu "attachments have differing sizes").
        let (w, h) = (v.width, v.height);
        bevy::winit::WINIT_WINDOWS.with_borrow(|ww| {
            for (_, window) in ww.windows.iter() {
                let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(w, h));
            }
        });
    }
    let p = present(v.vsync);
    if win.present_mode != p {
        win.present_mode = p;
    }
}

/// `FPSLimit`: sleeps out the rest of the frame budget.
fn frame_limit(mut next: Local<Option<std::time::Instant>>) {
    let n = with_video(|v| v.fps_limit);
    if n == 0 {
        *next = None;
        return;
    }
    let step = std::time::Duration::from_secs_f64(1.0 / n as f64);
    let now = std::time::Instant::now();
    let target = next.unwrap_or(now);
    if target > now {
        std::thread::sleep(target - now);
    }
    // Do not try to catch up after a long frame.
    *next = Some(target.max(now - step) + step);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_line_match() {
        assert!(is_version_line("Postal2: Steam-int-5100"));
        assert!(is_version_line("Postal2: Steam-int-5100(Oct 1 2020)"));
        assert!(!is_version_line("Copyright 2003-2015 RWS  All Rights Reserved"));
        assert!(!is_version_line("Postal2: hello"));
    }

    #[test]
    fn setres_forms() {
        assert_eq!(parse_setres("1920x1080 - 16:9xf"), Some((1920, 1080, Some(true))));
        assert_eq!(parse_setres("1280x720xw"), Some((1280, 720, Some(false))));
        assert_eq!(parse_setres("1600x900"), Some((1600, 900, None)));
        assert_eq!(parse_setres("junk"), None);
    }

    #[test]
    fn grade_identity_at_defaults() {
        assert!(Grade::of(&Video::default()).is_none());
        assert!(Grade::of(&Video { brightness: 0.8, ..Video::default() }).is_some());
    }
}
