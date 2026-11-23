struct BlurUniform {
    step_uv: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> u_blur: BlurUniform;

@group(0) @binding(1)
var u_tex: texture_2d<f32>;

@group(0) @binding(2)
var u_samp: sampler;

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    // Full-screen triangle covering clip space [-1, 1] and UV [0, 1]
    let x = f32((vid << 1u) & 2u);
    let y = f32(vid & 2u);
    var out: VsOut;
    out.clip_pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

@fragment
fn fs_blur(in: VsOut) -> @location(0) vec4<f32> {
    let d = u_blur.step_uv.xy;
    var acc = textureSampleLevel(u_tex, u_samp, in.uv, 0.0) * 0.227027;
    acc += textureSampleLevel(u_tex, u_samp, in.uv + d * 1.4, 0.0) * 0.1945946;
    acc += textureSampleLevel(u_tex, u_samp, in.uv - d * 1.4, 0.0) * 0.1945946;
    acc += textureSampleLevel(u_tex, u_samp, in.uv + d * 3.3, 0.0) * 0.1216216;
    acc += textureSampleLevel(u_tex, u_samp, in.uv - d * 3.3, 0.0) * 0.1216216;
    acc += textureSampleLevel(u_tex, u_samp, in.uv + d * 5.2, 0.0) * 0.054054;
    acc += textureSampleLevel(u_tex, u_samp, in.uv - d * 5.2, 0.0) * 0.054054;
    acc += textureSampleLevel(u_tex, u_samp, in.uv + d * 7.1, 0.0) * 0.0162162;
    acc += textureSampleLevel(u_tex, u_samp, in.uv - d * 7.1, 0.0) * 0.0162162;
    return vec4<f32>(acc.rgb, 1.0);
}

@fragment
fn fs_blit(in: VsOut) -> @location(0) vec4<f32> {
    let col = textureSampleLevel(u_tex, u_samp, in.uv, 0.0);
    return vec4<f32>(col.rgb, 1.0);
}
