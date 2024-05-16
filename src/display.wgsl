@group(0) @binding(0)
var in_sampler: sampler;
@group(0) @binding(1)
var in_texture: texture_2d<f32>;
@group(0) @binding(2)
var<uniform> display_settings: DisplaySettings;

struct DisplaySettings {
    min_uv: vec2f,
    max_uv: vec2f,
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

const VERTICES = array(
    //            pos             uvs
    array(vec2(-1.0,  1.0), vec2(0.0, 0.0)), // top left
    array(vec2( 1.0,  1.0), vec2(1.0, 0.0)), // top right
    array(vec2(-1.0, -1.0), vec2(0.0, 1.0)), // bottom left
    array(vec2( 1.0, -1.0), vec2(1.0, 1.0)), // bottom right
);

@vertex
fn vertex(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var verts = VERTICES; // needed for indexing with a variable; might be a naga limitation?

    var out: VertexOutput;
    out.position = vec4f(verts[vertex_index][0], 0.0, 1.0);
    out.uv = verts[vertex_index][1];
    out.uv = clamp(out.uv, display_settings.min_uv, display_settings.max_uv);

    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4f {
    let src = textureSample(in_texture, in_sampler, in.uv);

    // do a pre-multiplied alpha blend with the checkerboard colors
    let checkervec = vec2u(in.position.xy) / display_settings.checkerboard_res % 2; // even/odd in x/y dir
    let check = checkervec.x != checkervec.y;  // parity
    let dest = select(display_settings.checkerboard_a, display_settings.checkerboard_b, check);

    return src + ((1 - src.a) * dest);
}
