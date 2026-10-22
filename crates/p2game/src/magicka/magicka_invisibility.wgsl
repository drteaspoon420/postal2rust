// Original InvisibilityEffect: refract scene colour along the projected
// normal, limited by the depth gap behind the body. Distortion = -0.4.
#import bevy_pbr::{forward_io::VertexOutput, mesh_view_bindings as bindings,
    prepass_utils, view_transformations::depth_ndc_to_view_z}
@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> params: vec4<f32>;
@fragment
fn fragment(in: VertexOutput,
#ifdef MULTISAMPLED
    @builtin(sample_index) sample_index: u32,
#endif
) -> @location(0) vec4<f32> {
#ifndef MULTISAMPLED
    let sample_index = 0u;
#endif
    let size = vec2<f32>(textureDimensions(bindings::view_transmission_texture));
    let uv = in.position.xy / size;
    let own_depth = -depth_ndc_to_view_z(in.position.z);
    var scene_depth = own_depth + 2.0;
#ifdef DEPTH_PREPASS
    scene_depth = -depth_ndc_to_view_z(prepass_utils::prepass_depth(in.position, sample_index));
#endif
    let w = clamp((scene_depth-own_depth)*0.5,0.0,1.0)*params.x;
    let offset = vec2<f32>(w*0.02,w*0.02*size.x/size.y);
    let normal = normalize((bindings::view.clip_from_world*vec4<f32>(in.world_normal,0.0)).xyz);
    return vec4<f32>(textureSampleLevel(bindings::view_transmission_texture,
        bindings::view_transmission_sampler,clamp(uv+normal.xy*offset,vec2(0.0),vec2(1.0)),0.0).rgb,1.0);
}
