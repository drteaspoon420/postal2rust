//! VM binding: implements the Canvas, Interaction and InteractionMaster natives of the original
//! scripts on top of [`crate::canvas::Canvas`], so script HUD and menu code drives this renderer.
//!
//! - Canvas natives (`DrawText` 465, `StrLen` 464, `DrawTile` 466, `DrawActor` 467,
//!   `DrawTileClipped` 468, `DrawTextClipped` 469, `TextSize` 470, `DrawPortal` 480, `Draw3DLine`
//!   483, `WorldToScreen`, `ScreenToWorld`, `GetCameraLocation`) read the script Canvas object's
//!   fields (`OrgX`, `CurX`, `DrawColor`, `Font`, `Style`, ...) into the Rust canvas, run, and write
//!   the cursor fields (`CurX`, `CurY`, `CurYL`) back. Draws accumulate in [`CanvasContext`].
//! - Interaction natives: `Initialize` (calls the `Initialized` event), `ConsoleCommand` (routed to
//!   the console handler), `WorldToScreen`/`ScreenToWorld` (camera from the context).
//! - InteractionMaster: `Travel`, `LaunchDLC`, `LaunchSocialNetwork` (recorded / refused).
//! - [`MenuHost`]: builds the engine-side object graph that menus expect (Player/Viewport,
//!   InteractionMaster, Console and BaseMenu interactions created via the script
//!   `AddInteraction` event, as `UGameEngine::Init` / `UViewport` do) and drives a frame:
//!   `Process_Tick`, `Process_PreRender`/`Process_PostRender` with a script Canvas object, and
//!   key events through `Process_KeyEvent` / `Process_KeyType` (globals first, then the viewport's
//!   local interactions, as `MasterProcessKeyEvent` does).
//!
//! The natives are plain functions (the VM stores `fn` pointers), so the canvas state they draw into
//! lives in a thread-local [`CanvasContext`], installed with [`install`]. Hosts that also implement
//! [`uscript::host::Host`] can forward `resource_prop` to [`resource_prop`] for `Tex.USize`-style
//! reads of textures, materials and fonts.

use crate::canvas::{Canvas, MaterialSizer, Rgba};
use crate::eval::MaterialDb;
use crate::font::CanvasFont;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use upkg::library::{Library, Obj};
use uscript::Result;
use uscript::vm::{ObjId, Value, Vm};

/// Camera used by `WorldToScreen`/`ScreenToWorld`/`GetCameraLocation` (Unreal units/rotator).
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    pub location: [f32; 3],
    pub rotation: [i32; 3],
    /// Horizontal field of view in degrees.
    pub fov: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self { location: [0.0; 3], rotation: [0; 3], fov: 90.0 }
    }
}

/// Everything the Canvas natives need outside the VM.
pub struct CanvasContext {
    pub db: Arc<MaterialDb>,
    pub canvas: Canvas,
    pub camera: Camera,
    fonts: HashMap<String, Option<Arc<CanvasFont>>>,
    /// Console commands issued by script (`Interaction.ConsoleCommand`, `Travel`, ...), in order.
    pub commands: Vec<String>,
    /// Replies for console commands script queries (e.g. `GETFULLSCREEN`, `GetCurrentRes`).
    pub console: Option<Box<dyn FnMut(&str) -> Option<String>>>,
    /// Cursor warp requested by script (`SETMOUSE x y`, used by keyboard menu navigation). The
    /// host moves its cursor there and clears it.
    pub warp_mouse: Option<(f32, f32)>,
    /// `Canvas.DrawActor` requests this frame (first-person weapons, hands, the foot), drained by
    /// the host's 3D overlay renderer.
    pub actor_draws: Vec<ActorDraw>,
}

/// One `Canvas.DrawActor(A, WireFrame, ClearZ, DisplayFOV)` call: render `actor` (already placed
/// by script at its first-person Location/Rotation) with a camera at the canvas camera and the
/// given horizontal FOV, over the world. `items_before` is the number of 2D items queued before
/// the call, so a host can keep 2D/3D order if it wants to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ActorDraw {
    pub actor: Option<ObjId>,
    pub clear_z: bool,
    /// Degrees; 0 = the scene FOV.
    pub fov: f32,
    pub items_before: usize,
}

impl CanvasContext {
    pub fn new(lib: Arc<Library>, width: i32, height: i32) -> Self {
        Self {
            db: Arc::new(MaterialDb::new(lib)),
            canvas: Canvas::new(width, height),
            camera: Camera::default(),
            fonts: HashMap::new(),
            commands: Vec::new(),
            console: None,
            warp_mouse: None,
            actor_draws: Vec::new(),
        }
    }

    /// Font by object path (`P2Fonts.Plain24`, `Engine.SmallFont`).
    pub fn font(&mut self, path: &str) -> Option<Arc<CanvasFont>> {
        let key = path.to_ascii_lowercase();
        if let Some(f) = self.fonts.get(&key) {
            return f.clone();
        }
        let f = self.db.find(path, Some("Font")).and_then(|o| CanvasFont::load(&self.db.lib, &o));
        self.fonts.insert(key, f.clone());
        f
    }

    pub fn object(&self, path: &str) -> Option<Obj> {
        self.db.find(path, None)
    }
}

thread_local! {
    static CTX: RefCell<Option<CanvasContext>> = const { RefCell::new(None) };
}

/// Installs the context the natives draw into (replacing any previous one).
pub fn install(ctx: CanvasContext) {
    CTX.with(|c| *c.borrow_mut() = Some(ctx));
}

/// Runs `f` with the installed context. Panics if none is installed.
pub fn with_context<R>(f: impl FnOnce(&mut CanvasContext) -> R) -> R {
    CTX.with(|c| f(c.borrow_mut().as_mut().expect("p2canvas::natives::install was not called")))
}

/// Takes the installed context back out.
pub fn uninstall() -> Option<CanvasContext> {
    CTX.with(|c| c.borrow_mut().take())
}

fn has_context() -> bool {
    CTX.with(|c| c.borrow().is_some())
}

// ------------------------------------------------------------------ helpers

fn f(vm: &Vm, o: ObjId, n: &str) -> f32 {
    vm.get_prop(o, n).map(|v| v.as_float()).unwrap_or(0.0)
}

fn color_of(v: Option<&Value>) -> Rgba {
    match v {
        Some(Value::Struct(fields)) => {
            let g = |n: &str| fields.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.as_int().clamp(0, 255) as u8).unwrap_or(0);
            Rgba([g("R"), g("G"), g("B"), g("A")])
        }
        _ => Rgba([255, 255, 255, 255]),
    }
}

/// A material argument as a resource: textures embedded in script packages are instantiated as
/// VM objects (`Value::Object`), so they are mapped back to their package path.
fn material_arg(vm: &Vm, v: Option<&Value>) -> Option<Value> {
    match v {
        Some(Value::Object(Some(o))) => vm.subobject_path(*o).map(Value::Resource),
        v => v.cloned(),
    }
}

fn resource_path(v: &Value) -> Option<String> {
    match v {
        Value::Resource(p) => Some(p.clone()),
        Value::Str(s) | Value::Name(s) if !s.is_empty() && !s.eq_ignore_ascii_case("None") => Some(s.clone()),
        _ => None,
    }
}

/// Copies the script Canvas object's state into the Rust canvas.
fn pull(vm: &Vm, c: ObjId, ctx: &mut CanvasContext) {
    let font = vm.get_prop(c, "Font").and_then(resource_path);
    let font = font.and_then(|p| ctx.font(&p));
    let cv = &mut ctx.canvas;
    cv.font = font;
    cv.font_scale_x = vm.get_prop(c, "FontScaleX").map(|v| v.as_float()).filter(|v| *v != 0.0).unwrap_or(1.0);
    cv.font_scale_y = vm.get_prop(c, "FontScaleY").map(|v| v.as_float()).filter(|v| *v != 0.0).unwrap_or(1.0);
    cv.space_x = f(vm, c, "SpaceX");
    cv.space_y = f(vm, c, "SpaceY");
    cv.org_x = f(vm, c, "OrgX");
    cv.org_y = f(vm, c, "OrgY");
    cv.clip_x = f(vm, c, "ClipX");
    cv.clip_y = f(vm, c, "ClipY");
    cv.cur_x = f(vm, c, "CurX");
    cv.cur_y = f(vm, c, "CurY");
    cv.cur_yl = f(vm, c, "CurYL");
    cv.z = f(vm, c, "Z");
    cv.style = vm.get_prop(c, "Style").map(|v| v.as_int() as u8).unwrap_or(1);
    cv.draw_color = color_of(vm.get_prop(c, "DrawColor"));
    cv.b_center = vm.get_prop(c, "bCenter").is_some_and(|v| v.as_bool());
    cv.b_no_smooth = vm.get_prop(c, "bNoSmooth").is_some_and(|v| v.as_bool());
}

/// Writes the cursor back to the script Canvas object.
fn push(vm: &mut Vm, c: ObjId, cv: &Canvas) {
    vm.set_prop(c, "CurX", Value::Float(cv.cur_x));
    vm.set_prop(c, "CurY", Value::Float(cv.cur_y));
    vm.set_prop(c, "CurYL", Value::Float(cv.cur_yl));
}

/// Runs a canvas operation against the native's `self` Canvas object.
fn canvas_op<R>(vm: &mut Vm, op: impl FnOnce(&mut Canvas, &dyn MaterialSizer, &CanvasContextRefs) -> R) -> Option<R> {
    let c = vm.native_self()?;
    if !has_context() {
        return None;
    }
    let r = with_context(|ctx| {
        pull(vm, c, ctx);
        let db = ctx.db.clone();
        let refs = CanvasContextRefs { db: db.clone() };
        let r = op(&mut ctx.canvas, &*db, &refs);
        r
    });
    let cv = with_context(|ctx| ctx.canvas.clone_cursor());
    push(vm, c, &cv);
    Some(r)
}

/// Read-only lookups available inside a canvas operation.
pub struct CanvasContextRefs {
    pub db: Arc<MaterialDb>,
}

impl CanvasContextRefs {
    fn material(&self, v: Option<&Value>) -> Option<Obj> {
        v.and_then(resource_path).and_then(|p| self.db.find(&p, None))
    }
}

impl Canvas {
    /// A canvas carrying only the cursor fields (for writing back to script).
    fn clone_cursor(&self) -> Canvas {
        Canvas { cur_x: self.cur_x, cur_y: self.cur_y, cur_yl: self.cur_yl, items: Vec::new(), font: None, ..Canvas::new(self.size_x, self.size_y) }
    }
}

fn af(a: &[Value], k: usize) -> f32 {
    a.get(k).map(|v| v.as_float()).unwrap_or(0.0)
}

fn ab(a: &[Value], k: usize) -> bool {
    a.get(k).is_some_and(|v| v.as_bool())
}

fn astr(a: &[Value], k: usize) -> String {
    a.get(k).map(|v| v.as_str()).unwrap_or_default()
}

// ------------------------------------------------------------------ camera math

fn urad(u: i32) -> f32 {
    u as f32 * std::f32::consts::TAU / 65536.0
}

/// Unreal rotator to (forward, right, up) axes.
fn axes(r: [i32; 3]) -> [[f32; 3]; 3] {
    let (sp, cp) = urad(r[0]).sin_cos();
    let (sy, cy) = urad(r[1]).sin_cos();
    let (sr, cr) = urad(r[2]).sin_cos();
    let x = [cp * cy, cp * sy, sp];
    let y = [sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp];
    let z = [-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp];
    [x, y, z]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// UE2 `WorldToScreen`: screen X/Y in pixels, Z = depth along the view direction.
pub fn world_to_screen(cam: &Camera, size: (f32, f32), loc: [f32; 3]) -> [f32; 3] {
    let [x, y, z] = axes(cam.rotation);
    let d = [loc[0] - cam.location[0], loc[1] - cam.location[1], loc[2] - cam.location[2]];
    let (fx, fy, fz) = (dot(d, x), dot(d, y), dot(d, z));
    let half_w = size.0 * 0.5;
    let fov_scale = half_w / (cam.fov.to_radians() * 0.5).tan();
    if fx <= 0.0001 {
        return [-1.0, -1.0, fx];
    }
    [half_w + fy * fov_scale / fx, size.1 * 0.5 - fz * fov_scale / fx, fx]
}

/// UE2 `ScreenToWorld`: the offset from the camera to the screen pixel deprojected onto the near
/// clipping plane (5 uu along the view axis). The engine computes `SafeNormal()` of it but drops
/// the result, so the vector is NOT unit length; MapScreen/NewsScreen rely on that (they double it
/// and place the map arrow / newspaper there, "extremely close to the near clipping plane").
pub fn screen_to_world(cam: &Camera, size: (f32, f32), s: [f32; 3]) -> [f32; 3] {
    const NEAR_CLIPPING_PLANE: f32 = 5.0;
    let [x, y, z] = axes(cam.rotation);
    let half_w = size.0 * 0.5;
    let fov_scale = half_w / (cam.fov.to_radians() * 0.5).tan();
    let (sy, sz) = ((s[0] - half_w) / fov_scale, (size.1 * 0.5 - s[1]) / fov_scale);
    let d = [x[0] + y[0] * sy + z[0] * sz, x[1] + y[1] * sy + z[1] * sz, x[2] + y[2] * sy + z[2] * sz];
    [d[0] * NEAR_CLIPPING_PLANE, d[1] * NEAR_CLIPPING_PLANE, d[2] * NEAR_CLIPPING_PLANE]
}

fn camera_args(a: &[Value], first: usize, cam: Camera) -> Camera {
    let mut c = cam;
    if let Some(Value::Vector(v)) = a.get(first) {
        if *v != [0.0; 3] || matches!(a.get(first + 1), Some(Value::Rotator(_))) {
            c.location = *v;
        }
    }
    if let Some(Value::Rotator(r)) = a.get(first + 1) {
        c.rotation = *r;
    }
    c
}

// ------------------------------------------------------------------ registration

/// Registers every Canvas / Interaction / InteractionMaster native on the VM.
pub fn register(vm: &mut Vm) {
    // Canvas.
    vm.register_native("StrLen", |vm, a| {
        let s = astr(a, 0);
        if let Some((xl, yl)) = canvas_op(vm, |c, _, _| c.str_len(&s)) {
            if a.len() > 2 {
                a[1] = Value::Float(xl);
                a[2] = Value::Float(yl);
            }
        }
        Ok(Value::Void)
    });
    vm.register_native("TextSize", |vm, a| {
        let s = astr(a, 0);
        if let Some((xl, yl)) = canvas_op(vm, |c, _, _| c.text_size(&s)) {
            if a.len() > 2 {
                a[1] = Value::Float(xl);
                a[2] = Value::Float(yl);
            }
        }
        Ok(Value::Void)
    });
    vm.register_native("DrawText", |vm, a| {
        if is_scripted_texture(vm) {
            return Ok(Value::Void);
        }
        let (s, cr) = (astr(a, 0), ab(a, 1));
        canvas_op(vm, |c, sz, _| c.draw_text(sz, &s, cr));
        Ok(Value::Void)
    });
    vm.register_native("DrawTextClipped", |vm, a| {
        let (s, hk) = (astr(a, 0), ab(a, 1));
        if std::env::var_os("P2CANVAS_TRACE_TEXT").is_some() {
            let font = vm.native_self().and_then(|c| vm.get_prop(c, "Font").cloned());
            eprintln!("DrawTextClipped {s:?} font={font:?}");
        }
        canvas_op(vm, |c, sz, _| c.draw_text_clipped(sz, &s, hk));
        Ok(Value::Void)
    });
    vm.register_native("DrawTile", |vm, a| {
        if is_scripted_texture(vm) {
            return Ok(Value::Void);
        }
        let m = material_arg(vm, a.first());
        let (xl, yl, u, v, ul, vl) = (af(a, 1), af(a, 2), af(a, 3), af(a, 4), af(a, 5), af(a, 6));
        canvas_op(vm, |c, sz, r| {
            let mat = r.material(m.as_ref());
            c.draw_tile(sz, mat.as_ref(), xl, yl, u, v, ul, vl)
        });
        Ok(Value::Void)
    });
    vm.register_native("DrawTileClipped", |vm, a| {
        let m = material_arg(vm, a.first());
        let (xl, yl, u, v, ul, vl) = (af(a, 1), af(a, 2), af(a, 3), af(a, 4), af(a, 5), af(a, 6));
        canvas_op(vm, |c, sz, r| {
            let mat = r.material(m.as_ref());
            c.draw_tile_clipped(sz, mat.as_ref(), xl, yl, u, v, ul, vl)
        });
        Ok(Value::Void)
    });
    vm.register_native("DrawActor", |vm, a| {
        if is_scripted_texture(vm) || !has_context() {
            return Ok(Value::Void);
        }
        let actor = a.first().and_then(|v| v.as_obj());
        let clear_z = ab(a, 2);
        let fov = af(a, 3);
        with_context(|ctx| {
            let items_before = ctx.canvas.items.len();
            ctx.actor_draws.push(ActorDraw { actor, clear_z, fov, items_before });
        });
        Ok(Value::Void)
    });
    vm.register_native("DrawPortal", |_, _| Ok(Value::Void));
    vm.register_native("Draw3DLine", |vm, a| {
        // Projects both ends and draws a 1-pixel line as a thin tile of Engine.WhiteSquareTexture.
        let (p0, p1) = (a.first().map(|v| v.as_vec()).unwrap_or_default(), a.get(1).map(|v| v.as_vec()).unwrap_or_default());
        if !has_context() {
            return Ok(Value::Void);
        }
        let (cam, size) = with_context(|ctx| (ctx.camera, (ctx.canvas.size_x as f32, ctx.canvas.size_y as f32)));
        let (s0, s1) = (world_to_screen(&cam, size, p0), world_to_screen(&cam, size, p1));
        if s0[2] <= 0.0 || s1[2] <= 0.0 {
            return Ok(Value::Void);
        }
        canvas_op(vm, |c, sz, r| {
            let Some(white) = r.db.find("Engine.WhiteSquareTexture", None) else { return };
            let (dx, dy) = (s1[0] - s0[0], s1[1] - s0[1]);
            let steps = dx.abs().max(dy.abs()).ceil().max(1.0) as usize;
            let (ox, oy, cx, cy) = (c.org_x, c.org_y, c.cur_x, c.cur_y);
            c.org_x = 0.0;
            c.org_y = 0.0;
            for i in 0..=steps.min(4096) {
                let t = i as f32 / steps as f32;
                c.cur_x = s0[0] + dx * t;
                c.cur_y = s0[1] + dy * t;
                c.draw_tile(sz, Some(&white), 1.0, 1.0, 0.0, 0.0, 1.0, 1.0);
            }
            (c.org_x, c.org_y, c.cur_x, c.cur_y) = (ox, oy, cx, cy);
        });
        Ok(Value::Void)
    });
    vm.register_native("WorldToScreen", |_, a| {
        if !has_context() {
            return Ok(Value::Vector([0.0; 3]));
        }
        let loc = a.first().map(|v| v.as_vec()).unwrap_or_default();
        let (cam, size) = with_context(|ctx| (ctx.camera, (ctx.canvas.size_x as f32, ctx.canvas.size_y as f32)));
        Ok(Value::Vector(world_to_screen(&camera_args(a, 1, cam), size, loc)))
    });
    vm.register_native("ScreenToWorld", |_, a| {
        if !has_context() {
            return Ok(Value::Vector([1.0, 0.0, 0.0]));
        }
        let s = a.first().map(|v| v.as_vec()).unwrap_or_default();
        let (cam, size) = with_context(|ctx| (ctx.camera, (ctx.canvas.size_x as f32, ctx.canvas.size_y as f32)));
        Ok(Value::Vector(screen_to_world(&camera_args(a, 1, cam), size, s)))
    });
    vm.register_native("GetCameraLocation", |_, a| {
        if has_context() {
            let cam = with_context(|ctx| ctx.camera);
            if a.len() >= 2 {
                a[0] = Value::Vector(cam.location);
                a[1] = Value::Rotator(cam.rotation);
            }
        }
        Ok(Value::Void)
    });

    // Interaction.
    vm.register_native("Initialize", |vm, _| {
        if let Some(me) = vm.native_self() {
            vm.fire_event(me, "Initialized", vec![]);
        }
        Ok(Value::Void)
    });
    // Interaction.ConsoleCommand returns bool (Actor.ConsoleCommand, a string, stays with the host).
    // Both are registered under one name, so dispatch on the declaring class.
    vm.register_native("ConsoleCommand", |vm, a| {
        let cmd = astr(a, 0);
        let me = vm.native_self();
        let is_interaction = me.is_some_and(|o| vm.class_is_a(vm.objects[o].class, "Interactions"));
        let reply = console_command(vm, me, &cmd);
        Ok(if is_interaction { Value::Bool(reply.is_some()) } else { Value::Str(reply.unwrap_or_default()) })
    });

    // InteractionMaster.
    vm.register_native("Travel", |_, a| {
        let url = astr(a, 0);
        if has_context() {
            with_context(|ctx| ctx.commands.push(format!("TRAVEL {url}")));
        }
        Ok(Value::Void)
    });
    vm.register_native("LaunchDLC", |_, _| Ok(Value::Bool(false)));
    vm.register_native("LaunchSocialNetwork", |_, _| Ok(Value::Str("not supported".into())));
}

/// `ScriptedTexture` declares natives with the same names (DrawTile 473, DrawText 472); they draw
/// into a render target, not the canvas, and are not supported yet.
fn is_scripted_texture(vm: &Vm) -> bool {
    vm.native_self().is_some_and(|o| vm.class_is_a(vm.objects[o].class, "ScriptedTexture"))
}

/// Console commands from script. Input/menu queries are answered by the context's console
/// handler (e.g. a p2input `Input::console_query`); everything else goes to the VM host. Returns
/// the reply, or None if nobody handled it.
pub fn console_command(vm: &mut Vm, from: Option<ObjId>, cmd: &str) -> Option<String> {
    if std::env::var_os("P2CANVAS_TRACE_CMD").is_some() {
        eprintln!("CMD {cmd:?}");
    }
    if has_context() {
        let mut toks = cmd.split_whitespace();
        if toks.next().is_some_and(|v| v.eq_ignore_ascii_case("SETMOUSE")) {
            let x = toks.next().and_then(|s| s.parse::<f32>().ok());
            let y = toks.next().and_then(|s| s.parse::<f32>().ok());
            if let (Some(x), Some(y)) = (x, y) {
                with_context(|ctx| ctx.warp_mouse = Some((x, y)));
                return Some(String::new());
            }
        }
        let r = with_context(|ctx| {
            if ctx.commands.len() >= 256 {
                ctx.commands.drain(..128);
            }
            ctx.commands.push(cmd.to_string());
            ctx.console.as_mut().and_then(|h| h(cmd))
        });
        if r.is_some() {
            return r;
        }
    }
    let h = vm.host();
    let r = h.console_command(vm, from, cmd);
    (!r.is_empty()).then_some(r)
}

/// Answers `Host::resource_prop` for textures, materials and fonts (`USize`, `VSize`, `UClamp`,
/// `VClamp`, `UBits`, `VBits`, `MaterialUSize`/`MaterialVSize`, `Format`, `bMasked`, ...).
pub fn resource_prop(db: &MaterialDb, path: &str, prop: &str) -> Option<Value> {
    let obj = db.find(path, None)?;
    let class = obj.class().to_ascii_lowercase();
    let p = prop.to_ascii_lowercase();
    if matches!(p.as_str(), "usize" | "vsize" | "materialusize" | "materialvsize") {
        let (w, h) = db.size(&obj);
        return Some(Value::Int(if p.starts_with('u') || p == "materialusize" { w } else { h }));
    }
    let (_, props) = upkg::begin_object(&obj.pkg, obj.export).ok()?;
    let v = props.get(prop).cloned();
    Some(match v {
        Some(upkg::Value::Int(i)) => Value::Int(i),
        Some(upkg::Value::Float(f)) => Value::Float(f),
        Some(upkg::Value::Byte(b)) => Value::Byte(b),
        Some(upkg::Value::Bool(b)) => Value::Bool(b),
        Some(upkg::Value::Name(n)) => Value::Name(n),
        Some(upkg::Value::Str(s)) => Value::Str(s),
        Some(upkg::Value::Object(r)) => match db.lib.resolve(&obj.pkg, r) {
            Some(o) => Value::Resource(format!("{}.{}", o.pkg.name, o.name())),
            None => Value::Object(None),
        },
        _ => match (class.as_str(), p.as_str()) {
            (_, "uclamp") => Value::Int(db.size(&obj).0),
            (_, "vclamp") => Value::Int(db.size(&obj).1),
            (_, "ubits") => Value::Byte(db.size(&obj).0.max(1).ilog2() as u8),
            (_, "vbits") => Value::Byte(db.size(&obj).1.max(1).ilog2() as u8),
            (_, "bmasked" | "balphatexture" | "btwosided") => Value::Bool(false),
            _ => return None,
        },
    })
}

// ------------------------------------------------------------------ menu host

/// Engine-side object graph for the interaction system, plus per-frame driving.
pub struct MenuHost {
    pub player: ObjId,
    pub master: ObjId,
    pub console: Option<ObjId>,
    pub base_menu: Option<ObjId>,
    pub player_menu: Option<ObjId>,
    pub canvas: ObjId,
    pub player_controller: Option<ObjId>,
}

/// Classes for [`MenuHost::new`] (from `[Engine.Engine]` in Postal2.ini).
#[derive(Debug, Clone)]
pub struct MenuClasses {
    pub console: Option<String>,
    pub base_menu: Option<String>,
    pub default_player_menu: Option<String>,
}

impl MenuHost {
    /// Creates `Engine.Viewport` (the Player), `Engine.InteractionMaster` and a script `Canvas`,
    /// links them, then adds the Console (global interaction) and BaseMenu / DefaultPlayerMenu
    /// (local interactions of the viewport) through the script `AddInteraction` event.
    /// `pc` is the local PlayerController (`Player.Actor`), if one exists.
    pub fn new(vm: &mut Vm, classes: &MenuClasses, pc: Option<ObjId>) -> Result<Self> {
        let viewport_class = vm.find_class("Engine", "Viewport").or_else(|_| vm.find_class("Engine", "Player"))?;
        let player = vm.new_object(viewport_class);
        let im_class = vm.find_class("Engine", "InteractionMaster")?;
        let master = vm.new_object(im_class);
        let canvas_class = vm.find_class("Engine", "Canvas")?;
        let canvas = vm.new_object(canvas_class);
        vm.set_prop(player, "InteractionMaster", Value::Object(Some(master)));
        vm.set_prop(player, "Actor", Value::Object(pc));
        vm.set_prop(player, "bWindowsMouseAvailable", Value::Bool(true));
        if let Some(pc) = pc {
            vm.set_prop(pc, "Player", Value::Object(Some(player)));
        }
        let mut host = MenuHost { player, master, console: None, base_menu: None, player_menu: None, canvas, player_controller: pc };
        if let Some(c) = &classes.console {
            host.console = host.add_interaction(vm, c, None);
            vm.set_prop(master, "Console", Value::Object(host.console));
            vm.set_prop(player, "Console", Value::Object(host.console));
        }
        if let Some(m) = &classes.base_menu {
            host.base_menu = host.add_interaction(vm, m, Some(player));
            vm.set_prop(master, "BaseMenu", Value::Object(host.base_menu));
        }
        if let Some(m) = &classes.default_player_menu {
            host.player_menu = host.add_interaction(vm, m, Some(player));
        }
        Ok(host)
    }

    /// Links a PlayerController to the viewport (`PlayerController.SetPlayer`): call between
    /// `GameInfo.Login` and `PostLogin`, as the engine does, so `Possess` can add its screens.
    pub fn attach_player(&mut self, vm: &mut Vm, pc: ObjId) {
        vm.set_prop(self.player, "Actor", Value::Object(Some(pc)));
        vm.set_prop(pc, "Player", Value::Object(Some(self.player)));
        self.player_controller = Some(pc);
    }

    /// Script `InteractionMaster.AddInteraction(Name, AttachTo)`.
    pub fn add_interaction(&self, vm: &mut Vm, class: &str, attach_to: Option<ObjId>) -> Option<ObjId> {
        let r = vm.call_event(self.master, "AddInteraction", vec![Value::Str(class.into()), Value::Object(attach_to)]);
        match r {
            Ok(Some(v)) => v.as_obj(),
            Ok(None) => None,
            Err(e) => {
                vm.record_error("AddInteraction", &e);
                None
            }
        }
    }

    fn globals(&self, vm: &Vm) -> Value {
        vm.get_prop(self.master, "GlobalInteractions").cloned().unwrap_or(Value::Array(vec![]))
    }

    fn locals(&self, vm: &Vm) -> Value {
        vm.get_prop(self.player, "LocalInteractions").cloned().unwrap_or(Value::Array(vec![]))
    }

    /// Sets the script Canvas object's viewport fields for a new frame (as `UCanvas::Update`).
    pub fn reset_canvas(&self, vm: &mut Vm, width: i32, height: i32) {
        let c = self.canvas;
        for (k, v) in [
            ("SizeX", Value::Int(width)),
            ("SizeY", Value::Int(height)),
            ("ClipX", Value::Float(width as f32)),
            ("ClipY", Value::Float(height as f32)),
            ("OrgX", Value::Float(0.0)),
            ("OrgY", Value::Float(0.0)),
            ("CurX", Value::Float(0.0)),
            ("CurY", Value::Float(0.0)),
        ] {
            vm.set_prop(c, k, v);
        }
        vm.fire_event(c, "Reset", vec![]);
        vm.set_prop(c, "Viewport", Value::Object(Some(self.player)));
    }

    /// One frame: `Process_Tick` (globals, then locals), then PreRender/PostRender with the canvas.
    /// Locals render first and globals last (the console draws on top), each array in reverse order.
    pub fn frame(&self, vm: &mut Vm, dt: f32, width: i32, height: i32) {
        let (g, l) = (self.globals(vm), self.locals(vm));
        for arr in [&g, &l] {
            vm.fire_event(self.master, "Process_Tick", vec![arr.clone(), Value::Float(dt)]);
        }
        if has_context() {
            with_context(|ctx| ctx.canvas.begin_frame(width, height, ctx.canvas.time + dt));
        }
        self.reset_canvas(vm, width, height);
        let c = Value::Object(Some(self.canvas));
        for arr in [&l, &g] {
            vm.fire_event(self.master, "Process_PreRender", vec![arr.clone(), c.clone()]);
        }
        for arr in [&l, &g] {
            vm.fire_event(self.master, "Process_PostRender", vec![arr.clone(), c.clone()]);
        }
    }

    /// A key event (`MasterProcessKeyEvent`): globals first, then the viewport's locals. Returns
    /// true if an interaction consumed it (otherwise the host runs the key binding).
    pub fn key_event(&self, vm: &mut Vm, key: u8, action: u8, delta: f32) -> bool {
        for arr in [self.globals(vm), self.locals(vm)] {
            let r = vm.fire_event(self.master, "Process_KeyEvent", vec![arr, Value::Byte(key), Value::Byte(action), Value::Float(delta)]);
            if r.is_some_and(|v| v.as_bool()) {
                return true;
            }
        }
        false
    }

    /// A typed character (`MasterProcessKeyType`).
    pub fn key_type(&self, vm: &mut Vm, key: u8, unicode: &str) -> bool {
        for arr in [self.globals(vm), self.locals(vm)] {
            let r = vm.fire_event(self.master, "Process_KeyType", vec![arr, Value::Byte(key), Value::Str(unicode.into())]);
            if r.is_some_and(|v| v.as_bool()) {
                return true;
            }
        }
        false
    }

    /// Broadcasts a message to every interaction (`Process_Message`).
    pub fn message(&self, vm: &mut Vm, msg: &str, life: f32) {
        for arr in [self.globals(vm), self.locals(vm)] {
            vm.fire_event(self.master, "Process_Message", vec![Value::Str(msg.into()), Value::Float(life), arr]);
        }
    }

    /// Mouse position in pixels (`Player.WindowsMouseX/Y`, read by UWindow every frame).
    pub fn set_mouse(&self, vm: &mut Vm, x: f32, y: f32) {
        vm.set_prop(self.player, "WindowsMouseX", Value::Float(x));
        vm.set_prop(self.player, "WindowsMouseY", Value::Float(y));
    }

    /// Takes a pending script cursor warp (`SETMOUSE`), applying it to the viewport. The host
    /// should move the OS/virtual cursor to the returned position too.
    pub fn take_mouse_warp(&self, vm: &mut Vm) -> Option<(f32, f32)> {
        let w = if has_context() { with_context(|ctx| ctx.warp_mouse.take()) } else { None }?;
        self.set_mouse(vm, w.0, w.1);
        Some(w)
    }

    /// True while the root window wants a cursor (a menu is up: `bWindowsMouseAvailable` hosts
    /// only get a `SelectedCursor`, so the host draws the root's `MouseWindow.Cursor` itself).
    pub fn menu_active(&self, vm: &Vm) -> bool {
        self.base_menu.is_some_and(|r| {
            vm.get_prop(r, "bWindowVisible").is_some_and(|v| v.as_bool()) && vm.get_prop(r, "bUWindowActive").is_some_and(|v| v.as_bool())
        })
    }

    /// Draws the UWindow cursor (`Root.MouseWindow.Cursor`: tex, HotX, HotY) at the mouse.
    pub fn draw_cursor(&self, vm: &Vm) {
        let Some(root) = self.base_menu else { return };
        if !self.menu_active(vm) || !has_context() {
            return;
        }
        let mw = vm.get_prop(root, "MouseWindow").and_then(|v| v.as_obj()).unwrap_or(root);
        let cursor = vm.get_prop(mw, "Cursor").cloned();
        let field = |n: &str| match &cursor {
            Some(Value::Struct(f)) => f.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.clone()),
            _ => None,
        };
        let tex = field("tex").as_ref().and_then(resource_path).unwrap_or_else(|| "UWindow.Icons.MouseCursor".into());
        let (hx, hy) = (field("HotX").map(|v| v.as_float()).unwrap_or(0.0), field("HotY").map(|v| v.as_float()).unwrap_or(0.0));
        let scale = vm.get_prop(root, "GUIScale").map(|v| v.as_float()).filter(|s| *s > 0.0).unwrap_or(1.0);
        let mx = vm.get_prop(root, "MouseX").map(|v| v.as_float()).unwrap_or(0.0) * scale;
        let my = vm.get_prop(root, "MouseY").map(|v| v.as_float()).unwrap_or(0.0) * scale;
        with_context(|ctx| {
            let db = ctx.db.clone();
            let Some(mat) = db.find(&tex, None) else { return };
            let (w, h) = db.size(&mat);
            let cv = &mut ctx.canvas;
            let saved = (cv.org_x, cv.org_y, cv.clip_x, cv.clip_y, cv.style, cv.draw_color);
            cv.org_x = 0.0;
            cv.org_y = 0.0;
            cv.clip_x = cv.size_x as f32;
            cv.clip_y = cv.size_y as f32;
            cv.style = 5;
            cv.draw_color = Rgba([255, 255, 255, 255]);
            cv.cur_x = mx - hx;
            cv.cur_y = my - hy;
            cv.draw_tile(&*db, Some(&mat), w as f32, h as f32, 0.0, 0.0, w as f32, h as f32);
            (cv.org_x, cv.org_y, cv.clip_x, cv.clip_y, cv.style, cv.draw_color) = saved;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_screen_round_trip() {
        let cam = Camera { location: [0.0; 3], rotation: [0, 16384, 0], fov: 90.0 };
        // Looking down +Y; a point straight ahead is at the screen centre.
        let s = world_to_screen(&cam, (800.0, 600.0), [0.0, 100.0, 0.0]);
        assert!((s[0] - 400.0).abs() < 1e-3 && (s[1] - 300.0).abs() < 1e-3 && (s[2] - 100.0).abs() < 1e-3);
        let p = [30.0, 100.0, 20.0];
        let s = world_to_screen(&cam, (800.0, 600.0), p);
        let d = screen_to_world(&cam, (800.0, 600.0), s);
        // On the near plane (5 uu ahead), along the ray to p (p is 100 uu ahead).
        for k in 0..3 {
            assert!((d[k] - p[k] * 0.05).abs() < 1e-4);
        }
    }
}
