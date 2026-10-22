// UE2-style baked lighting for p2view.
//  - BSP: diffuse * lightmap atlas (UV1), lightmap in modulate-2x gamma space (128 = 1.0).
//  - Static meshes: diffuse * per-vertex baked light (vertex colour, already linear).
// Dynamic lighting is skipped (it is baked in). Distance fog still applies.
// Works in bindless mode (BakedLight index table at binding 100, slots 50/51) and in the
// per-material bind group mode (P2VIEW_NO_BINDLESS=1 or no driver support).

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
    pbr_bindings,
    mesh_bindings::mesh,
    mesh_view_bindings::view,
}
#import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d}

#ifdef BINDLESS
#import bevy_pbr::pbr_bindings::material_indices

struct BakedLightIndices {
    lightmap_texture: u32, // 50
    lightmap_sampler: u32, // 51
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<storage> baked_light_indices: array<BakedLightIndices>;
#else
@group(#{MATERIAL_BIND_GROUP}) @binding(50) var lightmap_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(51) var lightmap_sampler: sampler;
#endif

// Magicka classic camera cutaway (p2game magicka.rs `camera`): that camera sits at this fixed
// offset (Postal 0,600,712.5 uu) from the wizard and looks exactly back at them, so the wizard's
// position is recoverable here. Level surfaces above head height near the camera-to-wizard line
// are cut away so roofs and walls do not hide the ground being clicked on. Any other camera
// (first person, cinematics) never looks precisely along this direction.
const CLASSIC_OFFSET = vec3<f32>(0.0, 11.428572, 13.571428);
fn classic_cutaway(world: vec3<f32>) -> bool {
    let forward = -normalize(view.world_from_view[2].xyz);
    if dot(forward, -normalize(CLASSIC_OFFSET)) < 0.999999 {
        return false;
    }
    let wizard = view.world_position - CLASSIC_OFFSET;
    if world.y < wizard.y + 1.2 {
        return false;
    }
    let seg = view.world_position - wizard;
    let t = clamp(dot(world - wizard, seg) / dot(seg, seg), 0.0, 1.0);
    return distance(world, wizard + seg * t) < 4.5;
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
#ifdef BINDLESS
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
#endif
    if classic_cutaway(in.world_position.xyz) {
        discard;
    }
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    var light = vec3<f32>(1.0);
#ifdef VERTEX_COLORS
    // Bevy replaces base colour with the vertex colour; recover the texture colour and treat the
    // vertex colour as the baked light instead.
    light = in.color.rgb;
    var tex = vec4<f32>(1.0);
#ifdef VERTEX_UVS_A
#ifdef BINDLESS
    tex = textureSampleBias(
        bindless_textures_2d[material_indices[slot].base_color_texture],
        bindless_samplers_filtering[material_indices[slot].base_color_sampler],
        in.uv,
        -0.5,
    );
#else
    tex = textureSampleBias(pbr_bindings::base_color_texture, pbr_bindings::base_color_sampler, in.uv, -0.5);
#endif
#endif
    pbr_input.material.base_color = vec4<f32>(tex.rgb, tex.a);
#else
#ifdef VERTEX_UVS_B
#ifdef BINDLESS
    let lm = textureSample(
        bindless_textures_2d[baked_light_indices[slot].lightmap_texture],
        bindless_samplers_filtering[baked_light_indices[slot].lightmap_sampler],
        in.uv_b,
    ).rgb;
#else
    let lm = textureSample(lightmap_texture, lightmap_sampler, in.uv_b).rgb;
#endif
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
