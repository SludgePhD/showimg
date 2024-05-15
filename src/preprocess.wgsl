// Image preprocessing.
// Images that use an alpha channel have to be premultiplied in order to render/interpolate
// properly.

// (input and output are guaranteed to have the same dimensions)
@group(0) @binding(0)
var input: texture_2d<f32>;

@group(0) @binding(1)
var output: texture_storage_2d<rgba16float, read_write>;

@group(0) @binding(2)
var<storage, read_write> info: ImageInfo;

struct ImageInfo {
    uses_alpha: atomic<u32>, // 0 = no, 1 = yes
    known_straight: atomic<u32>, // 0 = possibly already premultiplied, 1 = definitely used straight alpha before preprocessing
}

@compute
@workgroup_size(16, 16)
fn preprocess(@builtin(global_invocation_id) gid: vec3u) {
    if any(gid.xy >= textureDimensions(input)) {
        return;
    }

    let pixel = textureLoad(input, gid.xy, 0); // full mip level

    let uses_alpha = pixel.a != 1.0;
    let known_straight = any(pixel.rgb > vec3(pixel.a));

    // if any pixel has an alpha value less than 1.0, the image uses its alpha channel
    atomicOr(&info.uses_alpha, u32(uses_alpha));

    // if any pixel's RGB values exceed the alpha value, the image is definitely using straight alpha
    atomicOr(&info.known_straight, u32(known_straight));

    var out = vec4(pixel.rgb * pixel.a, pixel.a);
    textureStore(output, gid.xy, out);
}
