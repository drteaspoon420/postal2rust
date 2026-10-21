// ArcaneEffect technique Beam, pixel shader (research/beams.md):
// c = (tex.r * ColorEdge + tex.g * ColorCenter) * alpha * tex.a,
// out = 1 - exp2(-c), alpha 1; blended SrcAlpha/One (additive).
#import bevy_pbr::forward_io::VertexOutput
@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> color_center: vec4<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var<uniform> color_edge: vec4<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var beams: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var beams_sampler: sampler;
fn to_raw(c: vec3<f32>) -> vec3<f32> {
    return select(1.055*pow(max(c,vec3(0.0)),vec3(1.0/2.4))-0.055,
                  c*12.92,c<=vec3(0.0031308));
}
fn to_lin(c: vec3<f32>) -> vec3<f32> {
    return select(pow((max(c,vec3(0.0))+0.055)/1.055,vec3(2.4)),
                  c/12.92,c<=vec3(0.04045));
}
@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    var alpha = 1.0;
#ifdef VERTEX_COLORS
    alpha = in.color.r;
#endif
    var texel = vec4(0.0);
#ifdef VERTEX_UVS_A
    let t = textureSample(beams, beams_sampler, in.uv);
    texel = vec4(to_raw(t.rgb), t.a);
#endif
    let c = (texel.r * color_edge.rgb + texel.g * color_center.rgb) * alpha * texel.a;
    return vec4(to_lin(vec3(1.0) - exp2(-c)), 1.0);
}
