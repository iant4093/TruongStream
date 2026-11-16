#include <metal_stdlib>
using namespace metal;

struct LutUniforms {
    float intensity;
    uint mirror_h;
    uint flip_v;
    float crop_zoom;
    float3 domain_min;
    float3 domain_max;
    uint output_width;
    uint output_height;
};

constant sampler linear_clamp(coord::normalized, address::clamp_to_edge, filter::linear);

inline float3 rgb_to_bt709_yuv(float3 rgb) {
    // BT.709 limited-range video encoding for standard NV12 virtual camera streams.
    float y  = 0.0625 + (0.1826 * rgb.r + 0.6142 * rgb.g + 0.0620 * rgb.b);
    float cb = 0.5000 + (-0.1006 * rgb.r - 0.3386 * rgb.g + 0.4392 * rgb.b);
    float cr = 0.5000 + (0.4392 * rgb.r - 0.3989 * rgb.g - 0.0403 * rgb.b);
    return clamp(float3(y, cb, cr), 0.0, 1.0);
}

kernel void apply_lut_and_convert_nv12(
    texture2d<float, access::sample> in_frame [[texture(0)]],
    texture3d<float, access::sample> lut_3d   [[texture(1)]],
    texture2d<float, access::write>  out_bgra [[texture(2)]],
    texture2d<float, access::write>  out_y    [[texture(3)]],
    texture2d<float, access::write>  out_uv   [[texture(4)]],
    constant LutUniforms&            u        [[buffer(0)]],
    uint2 gid [[thread_position_in_grid]]
) {
    if (gid.x >= u.output_width || gid.y >= u.output_height) {
        return;
    }

    float2 uv = (float2(gid) + 0.5) / float2(u.output_width, u.output_height);
    if (u.mirror_h != 0) {
        uv.x = 1.0 - uv.x;
    }
    if (u.flip_v != 0) {
        uv.y = 1.0 - uv.y;
    }

    // Preserve source aspect ratio (e.g., 3:2 EVF sensor) when scaling into 16:9 targets.
    float src_w = float(in_frame.get_width());
    float src_h = float(in_frame.get_height());
    float src_aspect = src_w / max(src_h, 1.0);
    float dst_aspect = float(u.output_width) / max(float(u.output_height), 1.0);
    float2 aspect_scale = float2(1.0, 1.0);
    if (dst_aspect > src_aspect) {
        aspect_scale.y = src_aspect / dst_aspect;
    } else if (src_aspect > dst_aspect) {
        aspect_scale.x = dst_aspect / src_aspect;
    }

    // Center-anchored aspect fit + digital crop zoom >= 1.0.
    float zoom = max(u.crop_zoom, 1.0);
    uv = (uv - 0.5) * (aspect_scale / zoom) + 0.5;

    float2 sample_uv = clamp(uv, 0.0, 1.0);
    float3 raw_rgb = in_frame.sample(linear_clamp, sample_uv).rgb;
    if (u.output_width > in_frame.get_width()) {
        // 5-tap Contrast-Adaptive Sharpening (CAS) reconstruction when upscaling USB EVF frames.
        float2 texel = 1.0 / max(float2(src_w, src_h), float2(1.0));
        float3 n = in_frame.sample(linear_clamp, clamp(sample_uv + float2(0.0, -texel.y), 0.0, 1.0)).rgb;
        float3 s = in_frame.sample(linear_clamp, clamp(sample_uv + float2(0.0,  texel.y), 0.0, 1.0)).rgb;
        float3 w = in_frame.sample(linear_clamp, clamp(sample_uv + float2(-texel.x, 0.0), 0.0, 1.0)).rgb;
        float3 e = in_frame.sample(linear_clamp, clamp(sample_uv + float2( texel.x, 0.0), 0.0, 1.0)).rgb;
        float3 min_rgb = min(min(raw_rgb, min(n, s)), min(w, e));
        float3 max_rgb = max(max(raw_rgb, max(n, s)), max(w, e));
        float3 amp = clamp(min(min_rgb, 1.0 - max_rgb) / max(max_rgb, float3(1e-4)), 0.0, 1.0);
        float3 weight = -0.14 * sqrt(amp);
        raw_rgb = clamp((raw_rgb + weight * (n + s + w + e)) / (1.0 + 4.0 * weight), 0.0, 1.0);
    }
    float3 lut_coord = (raw_rgb - u.domain_min) / max(u.domain_max - u.domain_min, float3(1e-5));
    float3 lut_rgb = lut_3d.sample(linear_clamp, clamp(lut_coord, 0.0, 1.0)).rgb;
    float3 graded = mix(raw_rgb, lut_rgb, clamp(u.intensity, 0.0, 1.0));

    out_bgra.write(float4(graded, 1.0), gid);

    float3 yuv = rgb_to_bt709_yuv(graded);
    out_y.write(float4(yuv.x, 0.0, 0.0, 1.0), gid);

    if ((gid.x & 1u) == 0u && (gid.y & 1u) == 0u) {
        out_uv.write(float4(yuv.y, yuv.z, 0.0, 1.0), gid >> 1u);
    }
}
