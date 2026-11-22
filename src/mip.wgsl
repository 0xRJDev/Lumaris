// Mipmap generation: each level is the 2x2 box filtered previous level. Image textures hold
// premultiplied alpha, so filtering never mixes in the colour of transparent pixels.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> VOut {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    var o: VOut;
    o.pos = vec4(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    o.uv = vec2(x, y);
    return o;
}

@fragment
fn fs_mip(in: VOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(src, s, in.uv, 0.0);
}

// Uploaded pixels (straight alpha, same size) to the premultiplied image texture
@fragment
fn fs_premultiply(in: VOut) -> @location(0) vec4<f32> {
    let c = textureLoad(src, vec2<i32>(in.pos.xy), 0);
    return vec4(c.rgb * c.a, c.a);
}
