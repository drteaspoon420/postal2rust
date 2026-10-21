// PolygonHead ParticleEffect, ported instruction by instruction from the
// disassembled vs_3_0 / ps_3_0 (output/evidence/particles/ParticleEffect.asm.txt,
// sampler states in ParticleEffect.states.txt). Register names are kept in
// comments. See output/research/particles.md.
//
// Copied from the Magicka rewrite viewer (magicka-viewer/src/particles.wgsl)
// for the Postal 2 host. Postal's renderer has no scene-depth snapshot, so the
// soft edge is omitted; the pipeline's depth test hides particles behind walls.
// Colors are computed in the game's raw space and written linearized.

#import bevy_pbr::mesh_view_bindings::view

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var sprites_a: texture_3d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var sprites_a_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var sprites_b: texture_3d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var sprites_b_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var sprites_c: texture_3d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(5) var sprites_c_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(6) var sprites_d: texture_3d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(7) var sprites_d_sampler: sampler;

struct Vertex {
    // Stream 1 Position1.xyz (particle centre).
    @location(0) center: vec3<f32>,
    // Stream 0 TexCoord0 (v1).
    @location(1) texcoord: vec2<f32>,
    // x: stream 0 Corner (v0.x); y: Position1.w rotation (v3.w);
    // z, w: Normal1 radius and normalized lifetime (v4.xy).
    @location(2) corner_rotation_radius_life: vec4<f32>,
    // Color1 (v5).
    @location(3) color: vec4<f32>,
    // TexCoord1 Byte4 (v6): sprite, alpha blended, HSV/colorize, aligned.
    @location(4) flags: vec4<f32>,
    // Normal0 velocity (v2).
    @location(5) velocity: vec3<f32>,
}

struct Varyings {
    @builtin(position) clip: vec4<f32>,  // o0
    @location(0) o1: vec4<f32>,          // uv, lifetime slice, sheet
    @location(1) o2: vec4<f32>,          // color rgb, faded alpha
    @location(2) o3: vec3<f32>,          // alpha blended, HSV, colorize
    @location(3) o4: vec2<f32>,          // view depth, radius
    @location(4) o6: vec3<f32>,          // hue rotation rows
    @location(5) o7: vec3<f32>,
    @location(6) o8: vec3<f32>,
}

const PI: f32 = 3.14159;        // c10.z (as stored)
const TWO_PI: f32 = 6.28319;    // c10.w
const INV_TWO_PI: f32 = 0.159155; // c11.x

fn flag(x: f32) -> f32 {
    // slt o, -abs(x), abs(x): 1 when x != 0.
    return select(0.0, 1.0, -abs(x) < abs(x));
}

@vertex
fn vertex(v: Vertex) -> Varyings {
    var out: Varyings;
    let corner = v.corner_rotation_radius_life.x;
    let rotation = v.corner_rotation_radius_life.y;
    let radius = v.corner_rotation_radius_life.z;
    let life = v.corner_rotation_radius_life.w;
    let view_from_world = view.view_from_world;

    // mad r0.w, v0.x, c8.x, c8.y
    var angle = corner * 1.5708 + 0.785398;
    if -abs(v.flags.w) < abs(v.flags.w) {
        // Rotation aligned: HLSL atan2 expansion over the view-space velocity.
        let r0xy = (view_from_world * vec4(v.velocity, 0.0)).xy;
        let len_inv = inverseSqrt(dot(vec3(r0xy, 0.0), vec3(r0xy, 0.0)));
        let r1 = r0xy * len_inv;
        let r0x = max(-r1.y, r1.y);
        let r0y = max(-r1.x, r1.x);
        let r0z = max(-r1.x, r1.y);
        var r1w = max(r0y, r0x);
        var r1z = 1.0 / r1w;
        r1w = min(r0y, r0x);
        r1z = r1z * r1w;
        let r2w = r1z * r1z;
        r1w = r2w * 0.0208351 + -0.085133;
        r1w = r2w * r1w + 0.180141;
        let r2z = r2w * r1w + -0.330299;
        r1w = min(-r1.x, r1.y);
        var r1y = r2w * r2z + 0.999866;
        r1w = select(0.0, 1.0, r1w < -r1w);
        r1y = r1z * r1y;
        r1z = select(0.0, 1.0, r0y < r0x);
        var a = r1y * -2.0 + 1.5708;
        let r0y2 = select(0.0, 1.0, -r1.x < r1.x);
        a = a * r1z + r1y;
        a = r0y2 * -3.14159 + a;
        var r0z2 = select(0.0, 1.0, r0z >= -r0z);
        r0z2 = r1w * r0z2;
        let twice = a + a;
        let aligned = r0z2 * -twice + a;
        angle = angle + aligned;
    } else {
        angle = angle + rotation + 1.5708;
    }

    // Sprite index → sheet, 8×8 cell (exact small-integer arithmetic).
    let sprite = floor(v.flags.x + 0.5);
    let sheet = floor(sprite / 64.0);
    let index = sprite - 64.0 * sheet;
    let column = index - 8.0 * floor(index / 8.0);
    let row = floor(index / 8.0);

    // mad r0.w, r0.w, c11.x, c11.y; frc; mad c10.w, c10.z; sincos
    let wrapped = fract(angle * INV_TWO_PI + 0.5) * TWO_PI - PI;
    let c = cos(wrapped);
    let s = sin(wrapped);
    let extent = 1.41421 * radius;
    let center_view = view_from_world * vec4(v.center, 1.0);
    let corner_view = vec4(s * extent + center_view.x, c * extent + center_view.y, center_view.z, center_view.w);
    let clip = view.clip_from_view * corner_view;
    out.clip = clip;

    out.o1 = vec4((vec2(column, row) + v.texcoord) * 0.125, life, sheet);
    // Fade: sat(8·t·(1−t)) times color alpha.
    let fade = saturate((1.0 - life) * life * 8.0);
    out.o2 = vec4(v.color.xyz, fade * v.color.w);
    out.o4 = vec2(-corner_view.z, radius);

    // HSV flags: v6.z is 50·HSV + 5·Colorize.
    let tenth = 0.1 * v.flags.z;
    let tenth_frac = fract(tenth);
    out.o3 = vec3(flag(v.flags.y), flag(tenth - tenth_frac), select(0.0, 1.0, -tenth_frac < tenth_frac));

    // Hue rotation about (1,1,1).
    let hue = fract(v.color.x * INV_TWO_PI + 0.5) * TWO_PI - PI;
    let hc = cos(hue);
    let hs = sin(hue);
    let one_minus = 1.0 - hc;
    let third = one_minus * 0.333333;
    let diag = one_minus * -0.666667 + 1.0;
    let a = hs * 0.57735 + third;
    let b = hs * -0.57735 + third;
    let r1 = vec3(a, b, diag);
    out.o8 = r1;
    out.o6 = r1.zxy;
    out.o7 = r1.yzx;
    return out;
}

fn to_lin(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((max(c, vec3(0.0)) + 0.055) / 1.055, vec3(2.4));
    return select(hi, lo, c <= vec3(0.04045));
}

@fragment
fn fragment(in: Varyings) -> @location(0) vec4<f32> {
    let uvw = in.o1.xyz;
    let sheet = floor(in.o1.w + 0.5);
    // cmp: exactly one sheet weight is 1.
    let wa = select(0.0, 1.0, -abs(sheet) >= 0.0);
    let wb = select(0.0, 1.0, -abs(sheet - 1.0) >= 0.0);
    let wc = select(0.0, 1.0, -abs(sheet - 2.0) >= 0.0);
    let wd = select(0.0, 1.0, -abs(sheet - 3.0) >= 0.0);
    // The raw bytes are sampled (Rgba8Unorm, no sRGB decode), as in D3D9.
    var r1 = textureSample(sprites_b, sprites_b_sampler, uvw) * wb;
    r1 = textureSample(sprites_a, sprites_a_sampler, uvw) * wa + r1;
    r1 = textureSample(sprites_c, sprites_c_sampler, uvw) * wc + r1;
    var r0 = textureSample(sprites_d, sprites_d_sampler, uvw) * wd + r1;

    if r0.w + -0.0001 < 0.0 {
        discard;
    }


    if in.o3.y != -in.o3.y {
        // HSV: optional colorize to luminance, hue rotation, saturation, value.
        let lum = dot(r0.xyz, vec3(0.3086, 0.6094, 0.082));
        let rgb = select(vec3(lum, 0.0, 0.0), r0.xyz, -in.o3.z >= 0.0);
        let h = vec3(dot(rgb, in.o6), dot(rgb, in.o7), dot(rgb, in.o8));
        let sat = in.o2.y;
        let col_x = vec3(0.3086 + (1.0 - 0.3086) * sat, sat * -0.6094 + 0.6094, sat * -0.082 + 0.082);
        let col_y = vec3(sat * -0.3086 + 0.3086, 0.6094 + (1.0 - 0.6094) * sat, sat * -0.082 + 0.082);
        let col_z = vec3(sat * -0.3086 + 0.3086, sat * -0.6094 + 0.6094, 0.082 + (1.0 - 0.082) * sat);
        r0 = vec4(vec3(dot(h, col_x), dot(h, col_y), dot(h, col_z)) * in.o2.z, r0.w * in.o2.w);
    } else {
        r0 = r0 * in.o2;
    }
    let rgb = r0.xyz * r0.w;
    // exp is base 2 in shader model 3.
    let raw = vec3(1.0) - exp2(-rgb);
    let alpha = select(1.0 - r0.w, 1.0, -in.o3.x >= 0.0);
    return vec4(to_lin(raw), alpha);
}
