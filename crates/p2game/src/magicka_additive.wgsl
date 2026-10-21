#import bevy_pbr::forward_io::VertexOutput
@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> tint: vec4<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var<uniform> flags: vec4<u32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var atlas: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var atlas_sampler: sampler;
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
    var sample = vec4(1.0);
#ifdef VERTEX_UVS_A
    if flags.x != 0u {
        let texel = textureSample(atlas,atlas_sampler,in.uv);
        sample = vec4(to_raw(texel.rgb),texel.a);
    }
#endif
    var color = vec4(1.0);
#ifdef VERTEX_COLORS
    if flags.y != 0u { color = in.color; }
#endif
    let value = sample*color*tint;
    let rgb = vec3(1.0)-exp2(-value.rgb*value.a);
    return vec4(to_lin(rgb),1.0);
}
