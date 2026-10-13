// Particle material for p2fx: UE2 particle blending on top of Bevy's mesh pipeline.
// Vertex data is already in world space (identity transform) and carries two sub-image UV sets
// plus a per-vertex blend factor for BlendBetweenSubdivisions.

#import bevy_pbr::mesh_functions::{get_world_from_local, mesh_position_local_to_clip}

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) uv_b: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) blend: f32,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) uv_b: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) blend: f32,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var tex: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var tex_sampler: sampler;
// x: alpha reference (0..1), y: alpha test on/off, z: draw style, w: has texture
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var<uniform> params: vec4<f32>;

@vertex
fn vertex(v: Vertex) -> VOut {
    var out: VOut;
    let m = get_world_from_local(v.instance_index);
    out.clip = mesh_position_local_to_clip(m, vec4<f32>(v.position, 1.0));
    out.uv = v.uv;
    out.uv_b = v.uv_b;
    out.color = v.color;
    out.blend = v.blend;
    return out;
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    return pow(max(c, vec3<f32>(0.0)), vec3<f32>(2.2));
}

@fragment
fn fragment(in: VOut) -> @location(0) vec4<f32> {
    var t = vec4<f32>(1.0);
    if params.w > 0.5 {
        t = textureSample(tex, tex_sampler, in.uv);
        if in.blend > 0.0 {
            t = mix(t, textureSample(tex, tex_sampler, in.uv_b), in.blend);
        }
    }
    let style = i32(params.z + 0.5);
    // Work in gamma space like the original fixed-function pipeline, convert at the end.
    let tg = vec4<f32>(pow(max(t.rgb, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2)), t.a);
    var g: vec4<f32>;
    if style == 2 {
        // Modulated: texture blended toward the vertex colour by vertex alpha (fade to grey).
        g = vec4<f32>(mix(in.color.rgb, tg.rgb, in.color.a), tg.a * in.color.a);
    } else {
        g = tg * in.color;
    }
    if params.y > 0.5 && g.a <= params.x {
        discard;
    }
    if style == 2 {
        // dst * 2 * src in gamma space == dst_linear * (2 src)^2.2; the blend state does 2*src*dst.
        return vec4<f32>(to_linear(2.0 * g.rgb) * 0.5, 1.0);
    }
    if style == 5 {
        // Darken: dst * (1 - src) in gamma space.
        return vec4<f32>(vec3<f32>(1.0) - to_linear(vec3<f32>(1.0) - g.rgb), g.a);
    }
    if style == 4 {
        // AlphaModulate: premultiplied (ONE, INVSRCALPHA).
        return vec4<f32>(to_linear(g.rgb), g.a);
    }
    if style == 6 {
        // Brighten (ONE, INVSRCCOLOR). UE2 blended in gamma space, s + d * (1 - s); in a linear
        // framebuffer the matching source term depends on the destination: about 0.7 s over
        // bright ground (thin urine/gas streams stay visible) and much less over dark scenes
        // (stacked fireballs must not wash out to white). s^1.6 sits between the two (sloth's
        // barrel-explosion and otter's urine references). The hue comes from the exact gamma
        // curve s^2.2 rescaled to the luminance of s^1.6: s^1.6 per channel lifted the weak
        // green/blue of fire sprites and washed explosions out to cream (FIDELITY #26/#45).
        let c = max(g.rgb, vec3<f32>(0.0));
        let w = vec3<f32>(0.2126, 0.7152, 0.0722);
        let bright = pow(c, vec3<f32>(1.6));
        let hue = pow(c, vec3<f32>(3.0));
        let k = dot(bright, w) / max(dot(hue, w), 1e-5);
        // Clip by the largest channel, not per channel: clamping red at 1 while green keeps its
        // gain turned the orange fireball core yellow, and stacked sprites then reached white
        // early (FIDELITY #45, orig stays orange).
        let v = hue * k;
        return vec4<f32>(v / max(max(v.r, max(v.g, v.b)), 1.0), g.a);
    }
    return vec4<f32>(to_linear(g.rgb), g.a);
}
