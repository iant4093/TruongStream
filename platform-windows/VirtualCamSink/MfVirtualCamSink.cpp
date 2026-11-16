#ifdef _WIN32
#include <windows.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mfvirtualcamera.h>
#include <d3d11.h>
#include <wrl/client.h>

#pragma comment(lib, "mf.lib")
#pragma comment(lib, "mfplat.lib")
#pragma comment(lib, "mfsensorgroup.lib")

namespace truongstream {

using Microsoft::WRL::ComPtr;

// CLSID matching registered Media Foundation Virtual Camera Media Source COM server.
static const wchar_t* kMediaSourceClsid = L"{8A2D5140-4A1E-4A92-9A54-7D83B8E01001}";

class MfVirtualCamSink {
public:
    HRESULT Start() {
        HRESULT hr = MFStartup(MF_VERSION);
        if (FAILED(hr)) return hr;

        hr = MFCreateVirtualCamera(
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
        sample->SetSampleDuration(166667); // 60 fps
        return S_OK;
    }

    void Stop() {
        if (virtualCam_) {
            virtualCam_->Stop();
            virtualCam_->Remove();
            virtualCam_.Reset();
        }
        MFShutdown();
    }

private:
    ComPtr<IMFVirtualCamera> virtualCam_;
};

} // namespace truongstream
#endif
