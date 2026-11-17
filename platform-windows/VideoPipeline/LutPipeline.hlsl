Texture2D<float4> InputFrame : register(t0);
Texture3D<float4> Lut3D      : register(t1);
SamplerState LinearClamp     : register(s0);

RWTexture2D<unorm float4> OutPreview : register(u0);
RWTexture2D<unorm float>  OutY       : register(u1);
RWTexture2D<unorm float2> OutUV      : register(u2);

cbuffer LutUniforms : register(b0) {
    float Intensity;
    uint MirrorH;
    uint FlipV;
    float CropZoom;
    float3 DomainMin;
    float _pad0;
    float3 DomainMax;
    uint OutputWidth;
    uint OutputHeight;
    uint3 _pad1;
};

float3 RgbToBt709Yuv(float3 rgb) {
    float y  = 0.0625 + (0.1826 * rgb.r + 0.6142 * rgb.g + 0.0620 * rgb.b);
    float cb = 0.5000 + (-0.1006 * rgb.r - 0.3386 * rgb.g + 0.4392 * rgb.b);
    float cr = 0.5000 + (0.4392 * rgb.r - 0.3989 * rgb.g - 0.0403 * rgb.b);
    return saturate(float3(y, cb, cr));
}

[numthreads(16, 16, 1)]
void CSMain(uint3 gid : SV_DispatchThreadID) {
    if (gid.x >= OutputWidth || gid.y >= OutputHeight) {
        return;
    }

    float2 uv = (float2(gid.xy) + 0.5) / float2(OutputWidth, OutputHeight);
    if (MirrorH != 0) uv.x = 1.0 - uv.x;
    if (FlipV != 0)   uv.y = 1.0 - uv.y;

    uint srcW = 0;
    uint srcH = 0;
    InputFrame.GetDimensions(srcW, srcH);
    float fSrcW = max(float(srcW), 1.0);
    float fSrcH = max(float(srcH), 1.0);
    float srcAspect = fSrcW / fSrcH;
    float dstAspect = float(OutputWidth) / max(float(OutputHeight), 1.0);
    float2 aspectScale = float2(1.0, 1.0);
    if (dstAspect > srcAspect) {
        aspectScale.y = srcAspect / dstAspect;
    } else if (srcAspect > dstAspect) {
        aspectScale.x = dstAspect / srcAspect;
    }

    float zoom = max(CropZoom, 1.0);
    float2 sampleUv = saturate((uv - 0.5) * (aspectScale / zoom) + 0.5);

    float3 rawRgb = InputFrame.SampleLevel(LinearClamp, sampleUv, 0).rgb;
    if (OutputWidth > srcW) {
        // 5-tap Contrast-Adaptive Sharpening (CAS) reconstruction when upscaling USB EVF frames.
        float2 texel = 1.0 / float2(fSrcW, fSrcH);
        float3 n = InputFrame.SampleLevel(LinearClamp, saturate(sampleUv + float2(0.0, -texel.y)), 0).rgb;
        float3 s = InputFrame.SampleLevel(LinearClamp, saturate(sampleUv + float2(0.0,  texel.y)), 0).rgb;
        float3 w = InputFrame.SampleLevel(LinearClamp, saturate(sampleUv + float2(-texel.x, 0.0)), 0).rgb;
        float3 e = InputFrame.SampleLevel(LinearClamp, saturate(sampleUv + float2( texel.x, 0.0)), 0).rgb;
        float3 minRgb = min(min(rawRgb, min(n, s)), min(w, e));
        float3 maxRgb = max(max(rawRgb, max(n, s)), max(w, e));
        float3 amp = saturate(min(minRgb, 1.0 - maxRgb) / max(maxRgb, float3(1e-4, 1e-4, 1e-4)));
        float3 weight = -0.14 * sqrt(amp);
        rawRgb = saturate((rawRgb + weight * (n + s + w + e)) / (1.0 + 4.0 * weight));
    }

    float3 lutCoord = saturate((rawRgb - DomainMin) / max(DomainMax - DomainMin, float3(1e-5, 1e-5, 1e-5)));
    float3 lutRgb = Lut3D.SampleLevel(LinearClamp, lutCoord, 0).rgb;
    float3 graded = lerp(rawRgb, lutRgb, saturate(Intensity));

    OutPreview[gid.xy] = float4(graded, 1.0);

    float3 yuv = RgbToBt709Yuv(graded);
    OutY[gid.xy] = yuv.x;
    if ((gid.x & 1u) == 0u && (gid.y & 1u) == 0u) {
        OutUV[gid.xy >> 1u] = yuv.yz;
    }
}
