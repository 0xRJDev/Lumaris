// Mipmap generation: each level is the 2x2 box filtered previous level.
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
