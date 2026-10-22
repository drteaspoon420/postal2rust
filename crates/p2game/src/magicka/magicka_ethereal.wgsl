// AnimatedModelAdditive: diffuse luminance * ColdColor * ethereal alpha,
// followed by the original 1-exp2(-intensity) transfer (viewer lit.wgsl).
#import bevy_pbr::forward_io::VertexOutput
@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> color: vec4<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var diffuse: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var diffuse_sampler: sampler;
fn raw(c: vec3<f32>) -> vec3<f32> {
    return select(1.055*pow(max(c,vec3(0.0)),vec3(1.0/2.4))-0.055, c*12.92,c<=vec3(0.0031308));
}
fn linear(c: vec3<f32>) -> vec3<f32> {
    return select(pow((max(c,vec3(0.0))+0.055)/1.055,vec3(2.4)),c/12.92,c<=vec3(0.04045));
}
@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    var texel = vec4<f32>(1.0);
#ifdef VERTEX_UVS_A
    texel = textureSample(diffuse, diffuse_sampler, in.uv);
#endif
    let lum = dot(raw(texel.rgb * color.rgb), vec3<f32>(0.3086,0.6094,0.082));
    let intensity = lum * vec3<f32>(1.0,1.6,2.0) * texel.a * color.a;
    return vec4<f32>(linear(vec3<f32>(1.0)-exp2(-intensity)),1.0);
}
