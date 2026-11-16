Texture2D<float4> InputFrame : register(t0);
Texture3D<float4> Lut3D      : register(t1);
SamplerState LinearClamp     : register(s0);

RWTexture2D<float4> OutPreview : register(u0);
RWTexture2D<unorm float> OutY  : register(u1);
RWTexture2D<unorm float2> OutUV : register(u2);

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

    float zoom = max(CropZoom, 1.0);
    uv = saturate((uv - 0.5) / zoom + 0.5);

    float3 rawRgb = InputFrame.SampleLevel(LinearClamp, uv, 0).rgb;
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
