// Ported from https://github.com/nvpro-samples/vk_compute_mipmaps/blob/main/nvpro_pyramid/nvpro_pyramid.glsl

@group(0) @binding(0)
var input: texture_2d<f32>;

@group(0) @binding(1)
var output: texture_storage_2d<rgba16float, write>;

override WORKGROUP_SIZE: u32 = 16;

@compute
@workgroup_size(WORKGROUP_SIZE, WORKGROUP_SIZE)
fn main(@builtin(global_invocation_id) gid: vec3u) {
    let srcSize = textureDimensions(input);
    let dstSize = textureDimensions(output);
    if any(gid.xy >= dstSize) {
        return;
    }

    let kernelSize = select(vec2(2) | (srcSize & vec2(1)), vec2(1), srcSize == vec2(1));
    let dstCoord = gid.xy;
    let srcCoord = dstCoord * 2;

    var n = f32(dstSize.y);
    var rcp = 1.0 / (2.0 * n + 1.0);
    var w0 = rcp * (n - f32(dstCoord.y));
    var w1 = rcp * n;
    var w2 = 1.0 - w0 - w1;

    var v0: vec4f;
    var v1: vec4f;
    var v2: vec4f;
    var h0: vec4f;
    var h1: vec4f;
    var h2: vec4f;

    if kernelSize.x == 3 {
        if kernelSize.y == 3 {
            v2 = loadSample(srcCoord + vec2(2, 2));
        }
        if kernelSize.y >= 2 {
            v1 = loadSample(srcCoord + vec2(2, 1));
        }
        v0 = loadSample(srcCoord + vec2(2, 0));

        switch kernelSize.y {
            case 3: {
                h2 = w0 * v0 + w1 * v1 + w2 * v2;
            }
            case 2: {
                h2 = 0.5 * (v0 + v1);
            }
            default: {
                h2 = v0;
            }
        }
    }
    if kernelSize.x >= 2 {
        if kernelSize.y == 3 {
            v2 = loadSample(srcCoord + vec2(1, 2));
        }
        if kernelSize.y >= 2 {
            v1 = loadSample(srcCoord + vec2(1, 1));
        }
        v0 = loadSample(srcCoord + vec2(1, 0));

        switch kernelSize.y {
            case 3: {
                h1 = w0 * v0 + w1 * v1 + w2 * v2;
            }
            case 2: {
                h1 = 0.5 * (v0 + v1);
            }
            default: {
                h1 = v0;
            }
        }
    }
    {
        if kernelSize.y == 3 {
            v2 = loadSample(srcCoord + vec2(0, 2));
        }
        if kernelSize.y >= 2 {
            v1 = loadSample(srcCoord + vec2(0, 1));
        }
        v0 = loadSample(srcCoord + vec2(0, 0));

        switch kernelSize.y {
            case 3: {
                h0 = w0 * v0 + w1 * v1 + w2 * v2;
            }
            case 2: {
                h0 = 0.5 * (v0 + v1);
            }
            default: {
                h0 = v0;
            }
        }
    }

    var out: vec4f;
    switch kernelSize.x {
        case 3: {
            n = f32(dstSize.x);
            rcp = 1.0 / (2.0 * n + 1.0);
            w0 = rcp * (n - f32(dstCoord.x));
            w1 = rcp * n;
            w2 = 1.0 - w0 - w1;

            out = w0 * h0 + w1 * h1 + w2 * h2;
        }
        case 2: {
            out = 0.5 * (h0 + h1);
        }
        default: {
            out = h0;
        }
    }

    textureStore(output, dstCoord, out);
}

fn loadSample(coords: vec2u) -> vec4f {
    return textureLoad(input, coords, 0);
}
