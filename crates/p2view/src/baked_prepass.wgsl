// Depth prepass for baked level surfaces: the standard alpha discard plus the Magicka classic
// camera cutaway (see baked.wgsl), so cut-away roofs and walls do not leave depth behind that
// hides what the main pass draws through them.
#import bevy_pbr::{
    prepass_io::VertexOutput,
    mesh_view_bindings::view,
}
#ifdef MAY_DISCARD
#import bevy_pbr::pbr_prepass_functions::prepass_alpha_discard
#endif

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
fn fragment(in: VertexOutput) {
#ifdef MAY_DISCARD
    prepass_alpha_discard(in);
#endif
    if classic_cutaway(in.world_position.xyz) {
        discard;
    }
}
