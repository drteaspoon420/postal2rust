// Magicka frozen look on Postal bodies: the skinned body pushed out along its
// normals ("bloat") into a translucent ice shell with a bright rim.
#import bevy_pbr::{
    mesh_functions,
    forward_io::{Vertex, VertexOutput},
    view_transformations::position_world_to_clip,
    mesh_view_bindings::view,
}
#ifdef SKINNED
#import bevy_pbr::skinning
#endif

// rgb: ice colour, w: bloat (metres).
@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> params: vec4<f32>;

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;
#ifdef SKINNED
    var world_from_local = skinning::skin_model(vertex.joint_indices, vertex.joint_weights, vertex.instance_index);
#else
    var world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);
#endif
    var n = vec3<f32>(0.0, 1.0, 0.0);
#ifdef VERTEX_NORMALS
#ifdef SKINNED
    n = normalize(skinning::skin_normals(world_from_local, vertex.normal));
#else
    n = normalize(mesh_functions::mesh_normal_local_to_world(vertex.normal, vertex.instance_index));
#endif
#endif
    out.world_normal = n;
    var wp = mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(vertex.position, 1.0));
    wp = vec4<f32>(wp.xyz + n * params.w, 1.0);
    out.world_position = wp;
    out.position = position_world_to_clip(wp.xyz);
#ifdef VERTEX_UVS_A
    out.uv = vertex.uv;
#endif
#ifdef VERTEX_UVS_B
    out.uv_b = vertex.uv_b;
#endif
#ifdef VERTEX_TANGENTS
    out.world_tangent = mesh_functions::mesh_tangent_local_to_world(world_from_local, vertex.tangent, vertex.instance_index);
#endif
#ifdef VERTEX_COLORS
    out.color = vertex.color;
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = vertex.instance_index;
#endif
#ifdef VISIBILITY_RANGE_DITHER
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(
        vertex.instance_index, world_from_local[3]);
#endif
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    var rim = 0.5;
#ifdef VERTEX_NORMALS
    let v = normalize(view.world_position - in.world_position.xyz);
    rim = 1.0 - abs(dot(normalize(in.world_normal), v));
#endif
    let alpha = clamp(0.28 + 0.6 * rim * rim, 0.0, 0.9);
    return vec4<f32>(params.rgb * (0.55 + 0.9 * rim), alpha);
}
