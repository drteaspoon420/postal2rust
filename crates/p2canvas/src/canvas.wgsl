// Canvas material: up to three sampled slots (texture or constant colour, each with its own UV
// transform), optionally combined with UE2 Combiner semantics, then tinted by the material tint
// and the Canvas DrawColor (vertex colour). Framebuffer blending is set by pipeline specialisation.
#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct CanvasMaterial {
    tint: vec4<f32>,
    slot_color0: vec4<f32>,
    slot_color1: vec4<f32>,
    slot_color2: vec4<f32>,
    // Row 0 of each UV transform: (m00, m01, t0, unused); row 1: (m10, m11, t1, unused).
    xu0: vec4<f32>,
    xv0: vec4<f32>,
    xu1: vec4<f32>,
    xv1: vec4<f32>,
    xu2: vec4<f32>,
    xv2: vec4<f32>,
    // Slot kinds: 0 empty, 1 texture, 2 constant colour (slot_colorN holds the colour).
    kinds: vec4<u32>,
    // color_op, alpha_op, invert_mask, mode (0 plain, 1 combiner, 2 shader opacity)
    ops: vec4<u32>,
    // modulate factor, alpha-test threshold (negative = off), unused, unused
    params: vec4<f32>,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> m: CanvasMaterial;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var tex0: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var smp0: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var tex1: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var smp1: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(5) var tex2: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(6) var smp2: sampler;

fn xf(uv: vec2<f32>, xu: vec4<f32>, xv: vec4<f32>) -> vec2<f32> {
    return vec2<f32>(xu.x * uv.x + xu.y * uv.y + xu.z, xv.x * uv.x + xv.y * uv.y + xv.z);
}

fn pick(kind: u32, sampled: vec4<f32>, color: vec4<f32>) -> vec4<f32> {
    if kind == 1u {
        return sampled * color;
    }
    if kind == 2u {
        return color;
    }
    return vec4<f32>(1.0, 1.0, 1.0, 1.0);
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Sample unconditionally (uniform control flow); unused slots bind a fallback image.
    let s0 = textureSample(tex0, smp0, xf(in.uv, m.xu0, m.xv0));
    let s1 = textureSample(tex1, smp1, xf(in.uv, m.xu1, m.xv1));
    let s2 = textureSample(tex2, smp2, xf(in.uv, m.xu2, m.xv2));
    let c1 = pick(m.kinds.x, s0, m.slot_color0);
    let c2 = pick(m.kinds.y, s1, m.slot_color1);
    let mk = pick(m.kinds.z, s2, m.slot_color2);

    var out = c1;
    if m.ops.w == 1u {
        var ma = mk.a;
        if m.ops.z == 1u {
            ma = 1.0 - ma;
        }
        var rgb = c1.rgb;
        switch m.ops.x {
            case 1u: { rgb = c2.rgb; }
            case 2u: { rgb = c1.rgb * c2.rgb; }
            case 3u: { rgb = c1.rgb + c2.rgb; }
            case 4u: { rgb = c1.rgb - c2.rgb; }
            case 5u: { rgb = mix(c2.rgb, c1.rgb, ma); }
            case 6u: { rgb = c1.rgb + c2.rgb * ma; }
            case 7u: { rgb = mk.rgb; }
            default: { rgb = c1.rgb; }
        }
        var a = ma;
        switch m.ops.y {
            case 1u: { a = c1.a * c2.a; }
            case 2u: { a = c1.a + c2.a; }
            case 3u: { a = c1.a; }
            case 4u: { a = c2.a; }
            default: { a = ma; }
        }
        out = vec4<f32>(clamp(rgb * m.params.x, vec3<f32>(0.0), vec3<f32>(1.0)), clamp(a, 0.0, 1.0));
    } else if m.ops.w == 2u {
        out = vec4<f32>(c1.rgb, mk.a);
    }

    var color = out * m.tint;
#ifdef VERTEX_COLORS
    color = color * in.color;
#endif
    if m.params.y >= 0.0 && color.a < m.params.y {
        discard;
    }
    return color;
}
