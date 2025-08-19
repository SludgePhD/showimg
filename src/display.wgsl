@group(0) @binding(0)
var in_sampler: sampler;
@group(0) @binding(1)
var in_texture: texture_2d<f32>;
@group(0) @binding(2)
var<uniform> u: DisplaySettings;

struct DisplaySettings {
    // min/max frame buffer coordinates to render within; everything else is checkerboard
    // this is used when the window aspect ratio doesn't match the image view's aspect ratio
    min_fb: vec2f,
    max_fb: vec2f,
    // image view UV coordinates to show
    min_uv: vec2f,
    max_uv: vec2f,
    // UV coordinates of the selection rectangle (both 0 if there's no selection)
    min_selection: vec2f,
    max_selection: vec2f,
    selection_color: vec4f,
    // checkerboard colors
    checkerboard_a: vec4f,
    checkerboard_b: vec4f,
    // width/height of each checkerboard square in output pixels
    checkerboard_res: u32,
    force_linear: u32, // 0 = smart filtering, 1 = always use linear filtering
    use_mipmaps: u32, // 0 = always show largest mip, 1 = auto
}

const MIN_SMOOTHNESS: f32 = 0.25;

struct VertexOutput {
    @builtin(position)
    position: vec4f,
};

const POSITIONS = array(
    vec2(-1.0,  1.0), // top left
    vec2( 1.0,  1.0), // top right
    vec2(-1.0, -1.0), // bottom left
    vec2( 1.0, -1.0), // bottom right
);

@vertex
fn vertex(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var pos = POSITIONS; // needed for indexing with a variable; might be a naga limitation?

    var out: VertexOutput;
    out.position = vec4f(pos[vertex_index], 0.0, 1.0);

    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4f {
    // FB coords of this fragment.
    let fb = in.position.xy;
    let border = any(fb < u.min_fb | fb >= u.max_fb);

    var uv = (fb - u.min_fb) / (u.max_fb - u.min_fb);

    // Map the UV coords (which are now in range 0 to 1) to the range indicated in the display settings.
    uv = (u.max_uv - u.min_uv) * uv + u.min_uv;

    if u.force_linear == 0 {
        // We want to render zoomed-in pixel art without making it all blurry, and without pixels getting
        // jittery when the window is enlarged. To do that, we use the approach detailed here:
        // https://csantosbh.wordpress.com/2014/01/25/manual-texture-filtering-for-pixelated-games-in-webgl/
        // We want the "smoothness" to be 1 when each texel occupies one or fewer window pixels, and
        // scale down to some minimum when each texel occupies more than one window pixel.
        // The size of each texel can be found out via derivatives.
        let dim = vec2f(textureDimensions(in_texture));
        let px = uv * dim; // sampled texture pixel
        let dxdy = abs(vec2(dpdxFine(px.x), dpdyFine(px.y)));
        let tex_per_px = max(dxdy.x, dxdy.y);
        // 1 or more texels per screen pixel? Full linear interpolation.
        // Less than 1? Gradually transition to nearest neighbor.
        let smoothness = clamp(tex_per_px, MIN_SMOOTHNESS, 1.0);

        var fract = fract(px);
        if smoothness == 0.0 {
            // Avoid division by zero. Zero smoothness means nearest-neighbor, so clamp the
            // coordinate to the pixel's center.
            fract = vec2(0.5);
        } else {
            fract = clamp(fract / smoothness, vec2(0.0), vec2(0.5))
                + clamp((fract - vec2(1.0)) / smoothness + 0.5, vec2(0.0), vec2(0.5));
        }

        uv = (floor(px) + fract) / dim;
    }

    let bias = select(-16.0, 0.0, u.use_mipmaps != 0);
    let tex_color = select(textureSampleBias(in_texture, in_sampler, uv, bias), vec4(0.0), border);

    // do a pre-multiplied alpha blend with the checkerboard colors
    let checkervec = vec2u(in.position.xy) / u.checkerboard_res % 2; // even/odd in x/y dir
    let check = checkervec.x != checkervec.y;  // parity
    var dest = select(u.checkerboard_a, u.checkerboard_b, check);

    dest = tex_color + (1 - tex_color.a) * dest;

    let in_selection = all(uv >= u.min_selection) && all(uv < u.max_selection);
    if in_selection {
        // blend the selection color on top
        let col = u.selection_color;
        dest = col + (1 - col.a) * dest;
    }

    return dest;
}
