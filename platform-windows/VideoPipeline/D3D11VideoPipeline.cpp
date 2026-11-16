#ifdef _WIN32
#include <d3d11_1.h>
#include <dxgi1_2.h>
#include <wrl/client.h>
#include <cstdint>
#include <vector>

#pragma comment(lib, "d3d11.lib")
#pragma comment(lib, "dxgi.lib")

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
};

class D3D11VideoPipeline {
public:
    HRESULT Initialize() {
        D3D_FEATURE_LEVEL level;
        return D3D11CreateDevice(
            nullptr,
            D3D_DRIVER_TYPE_HARDWARE,
            nullptr,
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            nullptr,
            0,
            D3D11_SDK_VERSION,
            &device_,
            &level,
            &context_
        );
    }

    HRESULT UploadLut3D(UINT size, const float* rgba32Data) {
        if (!device_) return E_POINTER;
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
        return device_->CreateShaderResourceView(tex.Get(), nullptr, &lutSrv_);
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
        desc.BindFlags = D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_UNORDERED_ACCESS;
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

private:
    ComPtr<ID3D11Device> device_;
    ComPtr<ID3D11DeviceContext> context_;
    ComPtr<ID3D11ShaderResourceView> lutSrv_;
};

} // namespace truongstream
#endif
