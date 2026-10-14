// Display grade (Options > Video Brightness / Contrast / Gamma), applied last over the whole
// frame (world, first-person weapon and HUD) by the canvas camera.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var texture_sampler: sampler;
struct Grade {
    offset: f32,
    slope: f32,
    inv_gamma: f32,
    _pad: f32,
}
@group(0) @binding(2) var<uniform> grade: Grade;

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let c = textureSample(screen_texture, texture_sampler, in.uv);
    // The target is sRGB: work on display values like the UE2 gamma ramp.
    var s = pow(max(c.rgb, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2));
    s = (s - vec3<f32>(0.5)) * grade.slope + vec3<f32>(0.5 + grade.offset);
    s = pow(clamp(s, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(grade.inv_gamma));
    return vec4<f32>(pow(s, vec3<f32>(2.2)), c.a);
}
