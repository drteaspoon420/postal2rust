// UE2-style baked lighting for p2view.
//  - BSP: diffuse * lightmap atlas (UV1), lightmap in modulate-2x gamma space (128 = 1.0).
//  - Static meshes: diffuse * per-vertex baked light (vertex colour, already linear).
// Dynamic lighting is skipped (it is baked in). Distance fog still applies.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
    pbr_bindings,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var lightmap_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var lightmap_sampler: sampler;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    var light = vec3<f32>(1.0);
#ifdef VERTEX_COLORS
    // Bevy replaces base colour with the vertex colour; recover the texture colour and treat the
    // vertex colour as the baked light instead.
    light = in.color.rgb;
    var tex = vec4<f32>(1.0);
#ifdef VERTEX_UVS_A
    tex = textureSampleBias(pbr_bindings::base_color_texture, pbr_bindings::base_color_sampler, in.uv, -0.5);
#endif
    pbr_input.material.base_color = vec4<f32>(tex.rgb, tex.a);
#else
#ifdef VERTEX_UVS_B
    let lm = textureSample(lightmap_texture, lightmap_sampler, in.uv_b).rgb;
    light = pow(lm * 2.0, vec3<f32>(2.2));
#endif
#endif
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);
    var out: FragmentOutput;
    // Tonemapping is off (UE2 writes straight to the framebuffer), so the 2x lightmap overbright
    // reaches the screen unclipped by Bevy's display curve. The original clamps the modulated
    // colour per channel in 8 bits: do the same so lit surfaces saturate instead of blowing out.
    out.color = vec4<f32>(min(pbr_input.material.base_color.rgb * light, vec3<f32>(1.0)), pbr_input.material.base_color.a);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
