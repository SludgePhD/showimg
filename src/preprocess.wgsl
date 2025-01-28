// Image preprocessing.
// Images that use an alpha channel have to be premultiplied in order to render/interpolate
// properly.

// (input and output are guaranteed to have the same dimensions)
@group(0) @binding(0)
var input: texture_2d<f32>;

@group(0) @binding(1)
var output: texture_storage_2d<rgba16float, write>;

@group(0) @binding(2)
var<storage, read_write> info: ImageInfo;

struct ImageInfo {
    uses_alpha: atomic<u32>, // 0 = every pixel has `alpha = 1.0`
    uses_partial_alpha: atomic<u32>, // 0 = every pixel has `alpha = 1.0` or `alpha = 0.0`
    known_straight: atomic<u32>, // 0 = possibly already premultiplied, 1 = definitely used straight alpha before preprocessing

    // offsets into the actual image content (ie. non-transparent region)
    top: atomic<u32>,
    right: atomic<u32>,
    bottom: atomic<u32>,
    left: atomic<u32>,
}

override WORKGROUP_SIZE: u32 = 16;

@compute
@workgroup_size(WORKGROUP_SIZE, WORKGROUP_SIZE)
fn preprocess(@builtin(global_invocation_id) gid: vec3u) {
    if any(gid.xy >= textureDimensions(input)) {
        return;
    }

    let pixel = textureLoad(input, gid.xy, 0); // full mip level

    let uses_alpha = pixel.a != 1.0;
    let uses_partial_alpha = pixel.a != 0.0 && pixel.a != 1.0;
    let known_straight = any(pixel.rgb > vec3(pixel.a));

    // if any pixel has an alpha value less than 1.0, the image uses its alpha channel
    atomicOr(&info.uses_alpha, u32(uses_alpha));

    // if any pixel has an alpha value between 0.0 and 1.0 (exclusive), the image uses "partial alpha"
    // if "partial alpha" is in use, the source image must not be already premultiplied, otherwise these pixels will look incorrect
    atomicOr(&info.uses_partial_alpha, u32(uses_partial_alpha));

    // if any pixel's RGB values exceed the alpha value, the image is definitely using straight alpha
    atomicOr(&info.known_straight, u32(known_straight));

    if any(pixel != vec4(0.0)) {
        // if the pixel contains *any* color, it is part of the image content, so update its
        // boundaries accordingly
        atomicMin(&info.left, gid.x);
        atomicMin(&info.top, gid.y);
        atomicMax(&info.right, gid.x);
        atomicMax(&info.bottom, gid.y);
    }

    var out = vec4(pixel.rgb * pixel.a, pixel.a);
    textureStore(output, gid.xy, out);
}
