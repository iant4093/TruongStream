#ifdef _WIN32
#include <windows.h>
#include <d3d11_1.h>
#include <dxgi1_2.h>
#include <d3dcompiler.h>
#include <wrl/client.h>
#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <mutex>
#include <vector>

namespace truongstream {

using Microsoft::WRL::ComPtr;

struct D3D11LutConstants {
    float intensity;
    uint32_t mirrorH;
    uint32_t flipV;
    float cropZoom;
    float domainMin[3];
    float pad0;
    float domainMax[3];
    uint32_t outputWidth;
    uint32_t outputHeight;
    uint32_t pad1[3];
};
static_assert(sizeof(D3D11LutConstants) == 64, "HLSL cbuffer requires 16-byte alignment");

class D3D11VideoPipeline {
public:
    HRESULT Initialize(const char* hlslSource) {
        D3D_FEATURE_LEVEL level = D3D_FEATURE_LEVEL_11_0;
        const D3D_FEATURE_LEVEL levels[] = {
            D3D_FEATURE_LEVEL_11_1,
            D3D_FEATURE_LEVEL_11_0,
        };
        HRESULT hr = D3D11CreateDevice(
            nullptr,
            D3D_DRIVER_TYPE_HARDWARE,
            nullptr,
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            levels,
            2,
            D3D11_SDK_VERSION,
            &device_,
            &level,
            &context_
        );
        if (FAILED(hr)) {
            hr = D3D11CreateDevice(
                nullptr,
                D3D_DRIVER_TYPE_WARP,
                nullptr,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                levels,
                2,
                D3D11_SDK_VERSION,
                &device_,
                &level,
                &context_
            );
        }
        if (FAILED(hr)) return hr;

        ComPtr<ID3DBlob> csBlob;
        ComPtr<ID3DBlob> errBlob;
        const size_t srcLen = hlslSource ? std::strlen(hlslSource) : 0;
        hr = D3DCompile(
            hlslSource,
            srcLen,
            "LutPipeline.hlsl",
            nullptr,
            nullptr,
            "CSMain",
            "cs_5_0",
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &csBlob,
            &errBlob
        );
        if (FAILED(hr)) return hr;

        hr = device_->CreateComputeShader(
            csBlob->GetBufferPointer(),
            csBlob->GetBufferSize(),
            nullptr,
            &computeShader_
        );
        if (FAILED(hr)) return hr;

        D3D11_SAMPLER_DESC samp{};
        samp.Filter = D3D11_FILTER_MIN_MAG_MIP_LINEAR;
        samp.AddressU = D3D11_TEXTURE_ADDRESS_CLAMP;
        samp.AddressV = D3D11_TEXTURE_ADDRESS_CLAMP;
        samp.AddressW = D3D11_TEXTURE_ADDRESS_CLAMP;
        samp.MaxLOD = D3D11_FLOAT32_MAX;
        hr = device_->CreateSamplerState(&samp, &linearClamp_);
        if (FAILED(hr)) return hr;

        D3D11_BUFFER_DESC cbDesc{};
        cbDesc.ByteWidth = sizeof(D3D11LutConstants);
        cbDesc.Usage = D3D11_USAGE_DEFAULT;
        cbDesc.BindFlags = D3D11_BIND_CONSTANT_BUFFER;
        return device_->CreateBuffer(&cbDesc, nullptr, &constantBuffer_);
    }

    HRESULT UploadLut3D(UINT size, const float* rgba32Data) {
        if (!device_ || !rgba32Data || size < 2) return E_INVALIDARG;
        std::lock_guard<std::mutex> lock(mutex_);

        D3D11_TEXTURE3D_DESC desc{};
        desc.Width = size;
        desc.Height = size;
        desc.Depth = size;
        desc.MipLevels = 1;
        desc.Format = DXGI_FORMAT_R32G32B32A32_FLOAT;
        desc.Usage = D3D11_USAGE_IMMUTABLE;
        desc.BindFlags = D3D11_BIND_SHADER_RESOURCE;

        D3D11_SUBRESOURCE_DATA init{};
        init.pSysMem = rgba32Data;
        init.SysMemPitch = size * sizeof(float) * 4;
        init.SysMemSlicePitch = init.SysMemPitch * size;

        ComPtr<ID3D11Texture3D> tex;
        HRESULT hr = device_->CreateTexture3D(&desc, &init, &tex);
        if (FAILED(hr)) return hr;
        ComPtr<ID3D11ShaderResourceView> srv;
        hr = device_->CreateShaderResourceView(tex.Get(), nullptr, &srv);
        if (SUCCEEDED(hr)) {
            lutSrv_ = srv;
        }
        return hr;
    }

    HRESULT CreateSharedNv12Target(UINT width, UINT height, ID3D11Texture2D** outTexture, HANDLE* outSharedHandle) {
        if (!device_) return E_POINTER;
        D3D11_TEXTURE2D_DESC desc{};
        desc.Width = width;
        desc.Height = height;
        desc.MipLevels = 1;
        desc.ArraySize = 1;
        desc.Format = DXGI_FORMAT_NV12;
        desc.SampleDesc.Count = 1;
        desc.Usage = D3D11_USAGE_DEFAULT;
        desc.BindFlags = D3D11_BIND_SHADER_RESOURCE;
        desc.MiscFlags = D3D11_RESOURCE_MISC_SHARED_NTHANDLE | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX;

        ComPtr<ID3D11Texture2D> tex;
        HRESULT hr = device_->CreateTexture2D(&desc, nullptr, &tex);
        if (FAILED(hr)) return hr;

        ComPtr<IDXGIResource1> dxgiRes;
        hr = tex.As(&dxgiRes);
        if (FAILED(hr)) return hr;

        hr = dxgiRes->CreateSharedHandle(
            nullptr,
            DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE,
            nullptr,
            outSharedHandle
        );
        if (SUCCEEDED(hr)) {
            *outTexture = tex.Detach();
        }
        return hr;
    }

    bool ProcessBgra(
        const uint8_t* bgraPtr,
        size_t bgraLen,
        uint32_t srcW,
        uint32_t srcH,
        uint32_t targetW,
        uint32_t targetH,
        float intensity,
        bool mirrorH,
        bool flipV,
        float cropZoom,
        const float domMin[3],
        const float domMax[3],
        bool wantNv12Bytes,
        bool wantPreviewRgb,
        uint32_t* outWidth,
        uint32_t* outHeight,
        float* outGpuMs,
        uint8_t** outYPtr,
        size_t* outYLen,
        uint8_t** outUvPtr,
        size_t* outUvLen,
        uint8_t** outRgbPtr,
        size_t* outRgbLen
    ) {
        if (!device_ || !context_ || !computeShader_ || !lutSrv_ || !bgraPtr) return false;
        if (srcW < 2 || srcH < 2 || bgraLen < static_cast<size_t>(srcW) * srcH * 4) return false;

        std::lock_guard<std::mutex> lock(mutex_);
        const auto t0 = std::chrono::steady_clock::now();

        const uint32_t outW = (std::max)(2u, ((targetW > 1 && targetH > 1) ? targetW : srcW) & ~1u);
        const uint32_t outH = (std::max)(2u, ((targetW > 1 && targetH > 1) ? targetH : srcH) & ~1u);

        if (FAILED(EnsureInputTexture(srcW, srcH))) return false;
        if (FAILED(EnsureOutputTargets(outW, outH))) return false;

        context_->UpdateSubresource(inputTex_.Get(), 0, nullptr, bgraPtr, srcW * 4, 0);

        D3D11LutConstants cb{};
        cb.intensity = intensity;
        cb.mirrorH = mirrorH ? 1u : 0u;
        cb.flipV = flipV ? 1u : 0u;
        cb.cropZoom = cropZoom;
        cb.domainMin[0] = domMin[0];
        cb.domainMin[1] = domMin[1];
        cb.domainMin[2] = domMin[2];
        cb.domainMax[0] = domMax[0];
        cb.domainMax[1] = domMax[1];
        cb.domainMax[2] = domMax[2];
        cb.outputWidth = outW;
        cb.outputHeight = outH;
        context_->UpdateSubresource(constantBuffer_.Get(), 0, nullptr, &cb, 0, 0);

        ID3D11ShaderResourceView* srvs[2] = { inputSrv_.Get(), lutSrv_.Get() };
        ID3D11UnorderedAccessView* uavs[3] = { outPreviewUav_.Get(), outYUav_.Get(), outUvUav_.Get() };
        ID3D11SamplerState* samplers[1] = { linearClamp_.Get() };
        ID3D11Buffer* cbs[1] = { constantBuffer_.Get() };

        context_->CSSetShader(computeShader_.Get(), nullptr, 0);
        context_->CSSetConstantBuffers(0, 1, cbs);
        context_->CSSetSamplers(0, 1, samplers);
        context_->CSSetShaderResources(0, 2, srvs);
        context_->CSSetUnorderedAccessViews(0, 3, uavs, nullptr);
        context_->Dispatch((outW + 15) / 16, (outH + 15) / 16, 1);

        ID3D11ShaderResourceView* nullSrvs[2] = { nullptr, nullptr };
        ID3D11UnorderedAccessView* nullUavs[3] = { nullptr, nullptr, nullptr };
        context_->CSSetShaderResources(0, 2, nullSrvs);
        context_->CSSetUnorderedAccessViews(0, 3, nullUavs, nullptr);

        if (wantNv12Bytes) {
            if (!ReadStagingBytes(outYTex_.Get(), outYStaging_.Get(), outW, outH, 1, outYPtr, outYLen)) {
                return false;
            }
            if (!ReadStagingBytes(outUvTex_.Get(), outUvStaging_.Get(), outW / 2, outH / 2, 2, outUvPtr, outUvLen)) {
                return false;
            }
        }

        if (wantPreviewRgb) {
            if (!ReadPreviewRgb8(outW, outH, outRgbPtr, outRgbLen)) {
                return false;
            }
        }

        if (outWidth) *outWidth = outW;
        if (outHeight) *outHeight = outH;
        if (outGpuMs) {
            const auto t1 = std::chrono::steady_clock::now();
            *outGpuMs = std::chrono::duration<float, std::milli>(t1 - t0).count();
        }
        return true;
    }

private:
    HRESULT EnsureInputTexture(uint32_t w, uint32_t h) {
        if (inputTex_ && inputW_ == w && inputH_ == h) return S_OK;
        inputTex_.Reset();
        inputSrv_.Reset();

        D3D11_TEXTURE2D_DESC desc{};
        desc.Width = w;
        desc.Height = h;
        desc.MipLevels = 1;
        desc.ArraySize = 1;
        desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
        desc.SampleDesc.Count = 1;
        desc.Usage = D3D11_USAGE_DEFAULT;
        desc.BindFlags = D3D11_BIND_SHADER_RESOURCE;

        HRESULT hr = device_->CreateTexture2D(&desc, nullptr, &inputTex_);
        if (FAILED(hr)) return hr;
        hr = device_->CreateShaderResourceView(inputTex_.Get(), nullptr, &inputSrv_);
        if (SUCCEEDED(hr)) {
            inputW_ = w;
            inputH_ = h;
        }
        return hr;
    }

    HRESULT CreateRwAndStaging(
        uint32_t w,
        uint32_t h,
        DXGI_FORMAT format,
        ComPtr<ID3D11Texture2D>& outGpu,
        ComPtr<ID3D11UnorderedAccessView>& outUav,
        ComPtr<ID3D11Texture2D>& outStage
    ) {
        D3D11_TEXTURE2D_DESC desc{};
        desc.Width = w;
        desc.Height = h;
        desc.MipLevels = 1;
        desc.ArraySize = 1;
        desc.Format = format;
        desc.SampleDesc.Count = 1;
        desc.Usage = D3D11_USAGE_DEFAULT;
        desc.BindFlags = D3D11_BIND_UNORDERED_ACCESS;

        HRESULT hr = device_->CreateTexture2D(&desc, nullptr, &outGpu);
        if (FAILED(hr)) return hr;
        hr = device_->CreateUnorderedAccessView(outGpu.Get(), nullptr, &outUav);
        if (FAILED(hr)) return hr;

        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
        return device_->CreateTexture2D(&desc, nullptr, &outStage);
    }

    HRESULT EnsureOutputTargets(uint32_t w, uint32_t h) {
        if (outPreviewTex_ && outW_ == w && outH_ == h) return S_OK;
        outPreviewTex_.Reset();
        outPreviewUav_.Reset();
        outPreviewStaging_.Reset();
        outYTex_.Reset();
        outYUav_.Reset();
        outYStaging_.Reset();
        outUvTex_.Reset();
        outUvUav_.Reset();
        outUvStaging_.Reset();

        HRESULT hr = CreateRwAndStaging(w, h, DXGI_FORMAT_R8G8B8A8_UNORM, outPreviewTex_, outPreviewUav_, outPreviewStaging_);
        if (FAILED(hr)) return hr;
        hr = CreateRwAndStaging(w, h, DXGI_FORMAT_R8_UNORM, outYTex_, outYUav_, outYStaging_);
        if (FAILED(hr)) return hr;
        hr = CreateRwAndStaging(w / 2, h / 2, DXGI_FORMAT_R8G8_UNORM, outUvTex_, outUvUav_, outUvStaging_);
        if (SUCCEEDED(hr)) {
            outW_ = w;
            outH_ = h;
        }
        return hr;
    }

    bool ReadStagingBytes(
        ID3D11Texture2D* gpuTex,
        ID3D11Texture2D* stagingTex,
        uint32_t w,
        uint32_t h,
        uint32_t bytesPerPixel,
        uint8_t** outPtr,
        size_t* outLen
    ) {
        if (!outPtr || !outLen) return true;
        context_->CopyResource(stagingTex, gpuTex);
        D3D11_MAPPED_SUBRESOURCE mapped{};
        if (FAILED(context_->Map(stagingTex, 0, D3D11_MAP_READ, 0, &mapped))) {
            return false;
        }
        const size_t rowBytes = static_cast<size_t>(w) * bytesPerPixel;
        const size_t total = rowBytes * h;
        uint8_t* dst = static_cast<uint8_t*>(std::malloc(total));
        if (!dst) {
            context_->Unmap(stagingTex, 0);
            return false;
        }
        const uint8_t* src = static_cast<const uint8_t*>(mapped.pData);
        for (uint32_t row = 0; row < h; ++row) {
            std::memcpy(dst + row * rowBytes, src + row * mapped.RowPitch, rowBytes);
        }
        context_->Unmap(stagingTex, 0);
        *outPtr = dst;
        *outLen = total;
        return true;
    }

    bool ReadPreviewRgb8(uint32_t w, uint32_t h, uint8_t** outPtr, size_t* outLen) {
        if (!outPtr || !outLen) return true;
        context_->CopyResource(outPreviewStaging_.Get(), outPreviewTex_.Get());
        D3D11_MAPPED_SUBRESOURCE mapped{};
        if (FAILED(context_->Map(outPreviewStaging_.Get(), 0, D3D11_MAP_READ, 0, &mapped))) {
            return false;
        }
        const size_t total = static_cast<size_t>(w) * h * 3;
        uint8_t* dst = static_cast<uint8_t*>(std::malloc(total));
        if (!dst) {
            context_->Unmap(outPreviewStaging_.Get(), 0);
            return false;
        }
        const uint8_t* src = static_cast<const uint8_t*>(mapped.pData);
        size_t k = 0;
        for (uint32_t y = 0; y < h; ++y) {
            const uint8_t* row = src + y * mapped.RowPitch;
            for (uint32_t x = 0; x < w; ++x) {
                dst[k++] = row[x * 4 + 0];
                dst[k++] = row[x * 4 + 1];
                dst[k++] = row[x * 4 + 2];
            }
        }
        context_->Unmap(outPreviewStaging_.Get(), 0);
        *outPtr = dst;
        *outLen = total;
        return true;
    }

    std::mutex mutex_;
    ComPtr<ID3D11Device> device_;
    ComPtr<ID3D11DeviceContext> context_;
    ComPtr<ID3D11ComputeShader> computeShader_;
    ComPtr<ID3D11SamplerState> linearClamp_;
    ComPtr<ID3D11Buffer> constantBuffer_;
    ComPtr<ID3D11ShaderResourceView> lutSrv_;

    uint32_t inputW_ = 0;
    uint32_t inputH_ = 0;
    ComPtr<ID3D11Texture2D> inputTex_;
    ComPtr<ID3D11ShaderResourceView> inputSrv_;

    uint32_t outW_ = 0;
    uint32_t outH_ = 0;
    ComPtr<ID3D11Texture2D> outPreviewTex_;
    ComPtr<ID3D11UnorderedAccessView> outPreviewUav_;
    ComPtr<ID3D11Texture2D> outPreviewStaging_;
    ComPtr<ID3D11Texture2D> outYTex_;
    ComPtr<ID3D11UnorderedAccessView> outYUav_;
    ComPtr<ID3D11Texture2D> outYStaging_;
    ComPtr<ID3D11Texture2D> outUvTex_;
    ComPtr<ID3D11UnorderedAccessView> outUvUav_;
    ComPtr<ID3D11Texture2D> outUvStaging_;
};

} // namespace truongstream

extern "C" {

void* ts_win_pipeline_create(const char* shaderSource) {
    auto* p = new truongstream::D3D11VideoPipeline();
    if (FAILED(p->Initialize(shaderSource))) {
        delete p;
        return nullptr;
    }
    return p;
}

void ts_win_pipeline_destroy(void* ptr) {
    delete static_cast<truongstream::D3D11VideoPipeline*>(ptr);
}

bool ts_win_pipeline_upload_lut(void* ptr, int32_t size, const float* rgba32Data) {
    if (!ptr || size < 2 || !rgba32Data) return false;
    return SUCCEEDED(static_cast<truongstream::D3D11VideoPipeline*>(ptr)->UploadLut3D(
        static_cast<UINT>(size), rgba32Data));
}

bool ts_win_pipeline_process_bgra(
    void* ptr,
    const uint8_t* bgraPtr,
    size_t bgraLen,
    uint32_t srcWidth,
    uint32_t srcHeight,
    uint32_t targetWidth,
    uint32_t targetHeight,
    float intensity,
    bool mirrorH,
    bool flipV,
    float cropZoom,
    float domMinR,
    float domMinG,
    float domMinB,
    float domMaxR,
    float domMaxG,
    float domMaxB,
    bool wantNv12Bytes,
    bool wantPreviewRgb,
    uint32_t* outWidth,
    uint32_t* outHeight,
    float* outGpuMs,
    uint8_t** outYPtr,
    size_t* outYLen,
    uint8_t** outUvPtr,
    size_t* outUvLen,
    uint8_t** outRgbPtr,
    size_t* outRgbLen
) {
    if (!ptr) return false;
    const float domMin[3] = { domMinR, domMinG, domMinB };
    const float domMax[3] = { domMaxR, domMaxG, domMaxB };
    return static_cast<truongstream::D3D11VideoPipeline*>(ptr)->ProcessBgra(
        bgraPtr,
        bgraLen,
        srcWidth,
        srcHeight,
        targetWidth,
        targetHeight,
        intensity,
        mirrorH,
        flipV,
        cropZoom,
        domMin,
        domMax,
        wantNv12Bytes,
        wantPreviewRgb,
        outWidth,
        outHeight,
        outGpuMs,
        outYPtr,
        outYLen,
        outUvPtr,
        outUvLen,
        outRgbPtr,
        outRgbLen
    );
}

void ts_win_free_buffer(uint8_t* ptr) {
    if (ptr) std::free(ptr);
}

} // extern "C"
#endif
