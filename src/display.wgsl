@group(0) @binding(0)
var in_sampler: sampler;
@group(0) @binding(1)
var in_texture: texture_2d<f32>;
@group(0) @binding(2)
var<uniform> display_settings: DisplaySettings;

struct DisplaySettings {
    checkerboard_a: vec4f,
    checkerboard_b: vec4f,
    checkerboard_res: u32,
}

struct VertexOutput {
    @builtin(position)
    position: vec4f,
    @location(0)
    uv: vec2f,
};

@vertex
fn vert(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // Logic copied from bevy's fullscreen quad shader
    var out: VertexOutput;
    out.uv = vec2f(f32(vertex_index >> 1), f32(vertex_index & 1)) * 2.0;
    out.position = vec4f(out.uv * vec2f(2.0, -2.0) + vec2f(-1.0, 1.0), 0.0, 1.0);
    return out;
}

@fragment
fn frag(in: VertexOutput) -> @location(0) vec4f {
    let src = textureSample(in_texture, in_sampler, in.uv);

    // do a pre-multiplied alpha blend with the checkerboard colors
    let checkervec = vec2u(in.position.xy) / display_settings.checkerboard_res % 2; // even/odd in x/y dir
    let check = checkervec.x != checkervec.y;  // parity
    let dest = select(display_settings.checkerboard_a, display_settings.checkerboard_b, check);

    return src + ((1 - src.a) * dest);
}
