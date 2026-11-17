#ifdef _WIN32
#include <windows.h>
#include <objbase.h>
#include <mfapi.h>
#include <mfidl.h>
#include <d3d11.h>
#include <wrl/client.h>
#include <cstdint>

#if __has_include(<mfvirtualcamera.h>)
#include <mfvirtualcamera.h>
#define TS_HAS_MF_VCAM_HEADER 1
#else
#define TS_HAS_MF_VCAM_HEADER 0
#endif

namespace truongstream {

using Microsoft::WRL::ComPtr;

// CLSID matching registered Media Foundation Virtual Camera Media Source COM server.
static const wchar_t* kMediaSourceClsid = L"{8A2D5140-4A1E-4A92-9A54-7D83B8E01001}";

class MfVirtualCamSink {
public:
    HRESULT Start() {
        CoInitializeEx(nullptr, COINIT_MULTITHREADED);
        HRESULT hr = MFStartup(MF_VERSION);
        if (FAILED(hr)) return hr;

#if TS_HAS_MF_VCAM_HEADER
        HMODULE mod = LoadLibraryW(L"mfsensorgroup.dll");
        if (!mod) {
            return HRESULT_FROM_WIN32(ERROR_MOD_NOT_FOUND);
        }
        using FnCreateVirtualCam = HRESULT(WINAPI*)(
            MFVirtualCameraType,
            MFVirtualCameraLifetime,
            MFVirtualCameraAccess,
            LPCWSTR,
            LPCWSTR,
            const GUID*,
            ULONG,
            IMFVirtualCamera**
        );
        auto fn = reinterpret_cast<FnCreateVirtualCam>(GetProcAddress(mod, "MFCreateVirtualCamera"));
        if (!fn) {
            FreeLibrary(mod);
            return E_NOTIMPL;
        }
        sensorGroupMod_ = mod;
        hr = fn(
            MFVirtualCameraType_SoftwareCameraSource,
            MFVirtualCameraLifetime_Session,
            MFVirtualCameraAccess_CurrentUser,
            L"TruongStream Virtual Camera",
            kMediaSourceClsid,
            nullptr,
            0,
            &virtualCam_
        );
        if (FAILED(hr)) return hr;
        return virtualCam_->Start(nullptr);
#else
        return E_NOTIMPL;
#endif
    }

    HRESULT PushSharedDxgiSurface(ID3D11Texture2D* nv12Texture, LONGLONG timestampHns) {
        if (!nv12Texture) return E_POINTER;
        ComPtr<IMFMediaBuffer> buffer;
        HRESULT hr = MFCreateDXGISurfaceBuffer(__uuidof(ID3D11Texture2D), nv12Texture, 0, FALSE, &buffer);
        if (FAILED(hr)) return hr;

        ComPtr<IMFSample> sample;
        hr = MFCreateSample(&sample);
        if (FAILED(hr)) return hr;

        hr = sample->AddBuffer(buffer.Get());
        if (FAILED(hr)) return hr;

        sample->SetSampleTime(timestampHns);
        sample->SetSampleDuration(166667);
        return S_OK;
    }

    void Stop() {
#if TS_HAS_MF_VCAM_HEADER
        if (virtualCam_) {
            virtualCam_->Stop();
            virtualCam_->Remove();
            virtualCam_.Reset();
        }
        if (sensorGroupMod_) {
            FreeLibrary(sensorGroupMod_);
            sensorGroupMod_ = nullptr;
        }
#endif
        MFShutdown();
    }

private:
#if TS_HAS_MF_VCAM_HEADER
    ComPtr<IMFVirtualCamera> virtualCam_;
    HMODULE sensorGroupMod_ = nullptr;
#endif
};

} // namespace truongstream

extern "C" {

void* ts_win_vcam_create() {
    return new truongstream::MfVirtualCamSink();
}

bool ts_win_vcam_start(void* ptr) {
    if (!ptr) return false;
    return SUCCEEDED(static_cast<truongstream::MfVirtualCamSink*>(ptr)->Start());
}

void ts_win_vcam_stop(void* ptr) {
    if (ptr) static_cast<truongstream::MfVirtualCamSink*>(ptr)->Stop();
}

void ts_win_vcam_destroy(void* ptr) {
    if (ptr) {
        auto* sink = static_cast<truongstream::MfVirtualCamSink*>(ptr);
        sink->Stop();
        delete sink;
    }
}

} // extern "C"
#endif
