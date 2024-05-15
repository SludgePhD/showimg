@group(0) @binding(0)
var in_sampler: sampler;
@group(0) @binding(1)
var in_texture: texture_2d<f32>;

struct VertexOutput {
    @builtin(position)
    position: vec4<f32>,
    @location(0)
    uv: vec2<f32>,
};

@vertex
fn vert(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // Logic copied from bevy's fullscreen quad shader
    var out: VertexOutput;
    out.uv = vec2<f32>(f32(vertex_index >> 1u), f32(vertex_index & 1u)) * 2.0;
    out.position = vec4<f32>(out.uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return out;
}

@fragment
fn frag(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(in_texture, in_sampler, in.uv);
}
