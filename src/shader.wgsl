// Draws the image quad. Filter by zoom:
//   mode 0: reduction - trilinear from box filtered mipmaps
//   mode 1: enlargement - Catmull-Rom bicubic (9 bilinear taps)
//   mode 2: pixel art enlargement - sharp bilinear (square pixels, antialiased edges)
//   mode 3: 1:1 - exact texels
struct U {
    rect: vec4<f32>,      // x0, y0, x1, y1 in window pixels
    viewport: vec2<f32>,
    tex_size: vec2<f32>,
    zoom: f32,
    mode: u32,
    lod: f32,
    pad: f32,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var s_lin: sampler;
@group(0) @binding(3) var s_near: sampler;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VOut {
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0));
    let c = corners[i];
    let px = mix(u.rect.xy, u.rect.zw, c);
    var o: VOut;
    o.pos = vec4(px.x / u.viewport.x * 2.0 - 1.0, 1.0 - px.y / u.viewport.y * 2.0, 0.0, 1.0);
    o.uv = c;
    return o;
}

fn bicubic(uv: vec2<f32>) -> vec4<f32> {
    let sz = u.tex_size;
    let sp = uv * sz;
    let tc = floor(sp - 0.5) + 0.5;
    let f = sp - tc;
    let w0 = f * (-0.5 + f * (1.0 - 0.5 * f));
    let w1 = 1.0 + f * f * (-2.5 + 1.5 * f);
    let w2 = f * (0.5 + f * (2.0 - 1.5 * f));
    let w3 = f * f * (-0.5 + 0.5 * f);
    let w12 = w1 + w2;
    let t0 = (tc - 1.0) / sz;
    let t3 = (tc + 2.0) / sz;
    let t12 = (tc + w2 / w12) / sz;
    var r = textureSampleLevel(tex, s_lin, vec2(t0.x, t0.y), 0.0) * w0.x * w0.y;
    r += textureSampleLevel(tex, s_lin, vec2(t12.x, t0.y), 0.0) * w12.x * w0.y;
    r += textureSampleLevel(tex, s_lin, vec2(t3.x, t0.y), 0.0) * w3.x * w0.y;
    r += textureSampleLevel(tex, s_lin, vec2(t0.x, t12.y), 0.0) * w0.x * w12.y;
    r += textureSampleLevel(tex, s_lin, vec2(t12.x, t12.y), 0.0) * w12.x * w12.y;
    r += textureSampleLevel(tex, s_lin, vec2(t3.x, t12.y), 0.0) * w3.x * w12.y;
    r += textureSampleLevel(tex, s_lin, vec2(t0.x, t3.y), 0.0) * w0.x * w3.y;
    r += textureSampleLevel(tex, s_lin, vec2(t12.x, t3.y), 0.0) * w12.x * w3.y;
    r += textureSampleLevel(tex, s_lin, vec2(t3.x, t3.y), 0.0) * w3.x * w3.y;
    return clamp(r, vec4(0.0), vec4(1.0));
}

fn sharp_pixels(uv: vec2<f32>) -> vec4<f32> {
    let px = uv * u.tex_size;
    let fl = floor(px);
    let f = clamp((px - fl - 0.5) * u.zoom + 0.5, vec2(0.0), vec2(1.0));
    return textureSampleLevel(tex, s_lin, (fl + f) / u.tex_size, 0.0);
}

fn sample(uv: vec2<f32>) -> vec4<f32> {
    switch u.mode {
        case 1u: { return bicubic(uv); }
        case 2u: { return sharp_pixels(uv); }
        case 3u: { return textureSampleLevel(tex, s_near, uv, 0.0); }
        default: { return textureSampleLevel(tex, s_lin, uv, u.lod); }
    }
}

// output with premultiplied alpha (the window itself may be see-through)
@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let c = sample(in.uv);
    return vec4(c.rgb * c.a, c.a);
}
