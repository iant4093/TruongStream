// Windows Portable Devices (WPD / IPortableDevice) raw PTP pass-through transport.
// Eliminates the need for Zadig / WinUSB driver replacement on Windows 10/11.

#ifdef _WIN32
#include <windows.h>
#include <PortableDeviceApi.h>
#include <PortableDevice.h>
#include <wrl/client.h>
#include <cstdint>
#include <string>
#include <vector>

#pragma comment(lib, "PortableDeviceGUIDs.lib")

namespace truongstream {

using Microsoft::WRL::ComPtr;

struct WpdPtpResult {
    uint16_t responseCode = 0;
    std::vector<uint8_t> data;
};

class WpdPtpTransport {
public:
    HRESULT OpenDevice(LPCWSTR deviceId) {
        HRESULT hr = CoCreateInstance(
            CLSID_PortableDeviceFTM, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&device_));
        if (FAILED(hr)) return hr;

        ComPtr<IPortableDeviceValues> clientInfo;
        hr = CoCreateInstance(
            CLSID_PortableDeviceValues, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&clientInfo));
        if (FAILED(hr)) return hr;

        clientInfo->SetStringValue(WPD_CLIENT_NAME, L"TruongStream");
        clientInfo->SetUnsignedIntegerValue(WPD_CLIENT_MAJOR_VERSION, 1);
        clientInfo->SetUnsignedIntegerValue(WPD_CLIENT_MINOR_VERSION, 0);
        clientInfo->SetUnsignedIntegerValue(WPD_CLIENT_SECURITY_QUALITY_OF_SERVICE, SECURITY_IMPERSONATION);

        return device_->Open(deviceId, clientInfo.Get());
    }

    HRESULT ExecuteNoData(uint16_t opcode, const std::vector<uint32_t>& params, uint16_t& outResponse) {
        return ExecuteInternal(
            WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITHOUT_DATA_PHASE,
            opcode, params, nullptr, 0, nullptr, &outResponse);
    }

    HRESULT ExecuteDataIn(uint16_t opcode, const std::vector<uint32_t>& params, WpdPtpResult& outResult) {
        return ExecuteInternal(
            WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITH_DATA_TO_READ,
            opcode, params, nullptr, 0, &outResult.data, &outResult.responseCode);
    }

    HRESULT ExecuteDataOut(
        uint16_t opcode,
        const std::vector<uint32_t>& params,
        const std::vector<uint8_t>& payload,
        uint16_t& outResponse) {
        return ExecuteInternal(
            WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITH_DATA_TO_WRITE,
            opcode, params, payload.data(), static_cast<DWORD>(payload.size()), nullptr, &outResponse);
    }

private:
    ComPtr<IPortableDevice> device_;

    HRESULT ExecuteInternal(
        const PROPERTYKEY& commandKey,
        uint16_t opcode,
        const std::vector<uint32_t>& params,
        const uint8_t* writeData,
        DWORD writeSize,
        std::vector<uint8_t>* readData,
        uint16_t* outResponse) {
        if (!device_) return E_POINTER;

        ComPtr<IPortableDeviceValues> cmdParams;
        HRESULT hr = CoCreateInstance(
            CLSID_PortableDeviceValues, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&cmdParams));
        if (FAILED(hr)) return hr;

        cmdParams->SetGuidValue(WPD_PROPERTY_COMMON_COMMAND_CATEGORY, commandKey.fmtid);
        cmdParams->SetUnsignedIntegerValue(WPD_PROPERTY_COMMON_COMMAND_ID, commandKey.pid);
        cmdParams->SetUnsignedIntegerValue(WPD_PROPERTY_MTP_EXT_OPERATION_CODE, opcode);

        ComPtr<IPortableDevicePropVariantCollection> ptpParams;
        hr = CoCreateInstance(
            CLSID_PortableDevicePropVariantCollection, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&ptpParams));
        if (FAILED(hr)) return hr;

        for (uint32_t p : params) {
            PROPVARIANT pv;
            PropVariantInit(&pv);
            pv.vt = VT_UI4;
            pv.ulVal = p;
            ptpParams->Add(&pv);
            PropVariantClear(&pv);
        }
        cmdParams->SetIPortableDevicePropVariantCollectionValue(
            WPD_PROPERTY_MTP_EXT_OPERATION_PARAMS, ptpParams.Get());

        if (writeData != nullptr && writeSize > 0) {
            cmdParams->SetBufferValue(WPD_PROPERTY_MTP_EXT_TRANSFER_DATA, const_cast<BYTE*>(writeData), writeSize);
        }

        ComPtr<IPortableDeviceValues> results;
        hr = device_->SendCommand(0, cmdParams.Get(), &results);
        if (FAILED(hr)) return hr;

        DWORD respCode = 0;
        results->GetUnsignedIntegerValue(WPD_PROPERTY_MTP_EXT_RESPONSE_CODE, &respCode);
        if (outResponse) *outResponse = static_cast<uint16_t>(respCode);

        if (readData) {
            BYTE* buf = nullptr;
            DWORD len = 0;
            if (SUCCEEDED(results->GetBufferValue(WPD_PROPERTY_MTP_EXT_TRANSFER_DATA, &buf, &len)) && buf) {
                readData->assign(buf, buf + len);
                CoTaskMemFree(buf);
            }
        }
        return S_OK;
    }
};

} // namespace truongstream
#endif
