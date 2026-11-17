#ifdef _WIN32
#include <windows.h>
#include <objbase.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mfreadwrite.h>
#include <mferror.h>
#include <wrl/client.h>
#include <algorithm>
#include <cctype>
#include <cmath>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <mutex>
#include <string>
#include <vector>

namespace truongstream {

using Microsoft::WRL::ComPtr;

static void EnsureMfInitialized() {
    CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    static std::once_flag mfOnce;
    std::call_once(mfOnce, []() { MFStartup(MF_VERSION); });
}

static std::string WideToUtf8(const wchar_t* wide) {
    if (!wide || !wide[0]) return {};
    int needed = WideCharToMultiByte(CP_UTF8, 0, wide, -1, nullptr, 0, nullptr, nullptr);
    if (needed <= 1) return {};
    std::string out(static_cast<size_t>(needed - 1), '\0');
    WideCharToMultiByte(CP_UTF8, 0, wide, -1, out.data(), needed, nullptr, nullptr);
    return out;
}

static std::wstring Utf8ToWide(const char* utf8) {
    if (!utf8 || !utf8[0]) return {};
    int needed = MultiByteToWideChar(CP_UTF8, 0, utf8, -1, nullptr, 0);
    if (needed <= 1) return {};
    std::wstring out(static_cast<size_t>(needed - 1), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, utf8, -1, out.data(), needed);
    return out;
}

static bool IsIgnoredVirtualDevice(const std::string& name) {
    std::string lower = name;
    for (char& ch : lower) ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
    return lower.find("truongstream") != std::string::npos ||
           lower.find("obs virtual") != std::string::npos;
}

static void InspectMaxCapabilities(IMFActivate* activate, uint32_t& outW, uint32_t& outH, uint32_t& outFps) {
    outW = 1920;
    outH = 1080;
    outFps = 60;

    ComPtr<IMFMediaSource> source;
    if (FAILED(activate->ActivateObject(IID_PPV_ARGS(&source))) || !source) return;

    ComPtr<IMFPresentationDescriptor> pd;
    if (FAILED(source->CreatePresentationDescriptor(&pd)) || !pd) {
        source->Shutdown();
        return;
    }

    BOOL selected = FALSE;
    ComPtr<IMFStreamDescriptor> sd;
    if (FAILED(pd->GetStreamDescriptorByIndex(0, &selected, &sd)) || !sd) {
        source->Shutdown();
        return;
    }

    ComPtr<IMFMediaTypeHandler> handler;
    if (SUCCEEDED(sd->GetMediaTypeHandler(&handler)) && handler) {
        DWORD count = 0;
        handler->GetMediaTypeCount(&count);
        uint32_t bestArea = 0;
        for (DWORD i = 0; i < count; ++i) {
            ComPtr<IMFMediaType> mt;
            if (FAILED(handler->GetMediaTypeByIndex(i, &mt)) || !mt) continue;
            UINT32 w = 0, h = 0;
            if (FAILED(MFGetAttributeSize(mt.Get(), MF_MT_FRAME_SIZE, &w, &h))) continue;
            UINT32 num = 60, den = 1;
            MFGetAttributeRatio(mt.Get(), MF_MT_FRAME_RATE, &num, &den);
            uint32_t fps = den > 0 ? (std::max)(15u, num / den) : 60u;
            uint64_t area = static_cast<uint64_t>(w) * h;
            if (area > bestArea || (area == bestArea && fps > outFps)) {
                bestArea = static_cast<uint32_t>(area);
                outW = w;
                outH = h;
                outFps = fps;
            }
        }
    }

    source->Shutdown();
}

class MfHdmiCaptureSession {
public:
    ~MfHdmiCaptureSession() { Stop(); }

    bool Open(const char* deviceIdUtf8, uint32_t width, uint32_t height, uint32_t fps) {
        EnsureMfInitialized();
        const std::wstring targetLink = Utf8ToWide(deviceIdUtf8);

        ComPtr<IMFAttributes> enumAttrs;
        if (FAILED(MFCreateAttributes(&enumAttrs, 1))) return false;
        enumAttrs->SetGUID(
            MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID
        );

        IMFActivate** devices = nullptr;
        UINT32 count = 0;
        if (FAILED(MFEnumDeviceSources(enumAttrs.Get(), &devices, &count)) || !devices) return false;

        ComPtr<IMFActivate> chosen;
        for (UINT32 i = 0; i < count; ++i) {
            WCHAR* link = nullptr;
            UINT32 linkLen = 0;
            if (SUCCEEDED(devices[i]->GetAllocatedString(
                    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, &link, &linkLen)) && link) {
                if (targetLink == link) {
                    chosen = devices[i];
                }
                CoTaskMemFree(link);
            }
            if (!chosen && i == 0 && targetLink.empty()) {
                chosen = devices[i];
            }
            devices[i]->Release();
        }
        CoTaskMemFree(devices);
        if (!chosen) return false;

        ComPtr<IMFMediaSource> source;
        if (FAILED(chosen->ActivateObject(IID_PPV_ARGS(&source))) || !source) return false;
        mediaSource_ = source;

        ComPtr<IMFAttributes> readerAttrs;
        MFCreateAttributes(&readerAttrs, 1);
        if (readerAttrs) {
            readerAttrs->SetUINT32(MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, TRUE);
        }

        if (FAILED(MFCreateSourceReaderFromMediaSource(mediaSource_.Get(), readerAttrs.Get(), &reader_))) {
            Stop();
            return false;
        }

        return ConfigureFormat(width, height, fps);
    }

    bool ConfigureFormat(uint32_t targetW, uint32_t targetH, uint32_t targetFps) {
        if (!reader_) return false;
        std::lock_guard<std::mutex> lock(mutex_);

        ComPtr<IMFMediaType> bestNative;
        int64_t bestScore = INT64_MIN;
        uint32_t chosenW = 1920;
        uint32_t chosenH = 1080;
        int32_t chosenStride = 1920 * 4;

        for (DWORD idx = 0;; ++idx) {
            ComPtr<IMFMediaType> mt;
            HRESULT hr = reader_->GetNativeMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM, idx, &mt);
            if (hr == MF_E_NO_MORE_TYPES || FAILED(hr) || !mt) break;

            UINT32 w = 0, h = 0;
            if (FAILED(MFGetAttributeSize(mt.Get(), MF_MT_FRAME_SIZE, &w, &h))) continue;
            UINT32 num = 60, den = 1;
            MFGetAttributeRatio(mt.Get(), MF_MT_FRAME_RATE, &num, &den);
            double fps = den > 0 ? static_cast<double>(num) / den : 60.0;

            GUID subtype = GUID_NULL;
            mt->GetGUID(MF_MT_SUBTYPE, &subtype);
            bool isUncompressed = (subtype != MFVideoFormat_MJPG);

            int64_t score = 0;
            if (w == targetW && h == targetH) {
                score += 100000;
            } else {
                int64_t diff = std::abs(static_cast<int64_t>(w) * h - static_cast<int64_t>(targetW) * targetH);
                score -= diff / 1000;
            }
            if (isUncompressed) score += 5000;
            if (fps >= targetFps) {
                score += 1000;
            } else {
                score += static_cast<int64_t>(fps * 10.0);
            }

            if (score > bestScore) {
                bestScore = score;
                bestNative = mt;
                chosenW = w;
                chosenH = h;
            }
        }

        if (bestNative) {
            reader_->SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM, nullptr, bestNative.Get());
        }

        ComPtr<IMFMediaType> outType;
        if (SUCCEEDED(MFCreateMediaType(&outType))) {
            outType->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video);
            outType->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_RGB32);
            reader_->SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM, nullptr, outType.Get());
        }

        ComPtr<IMFMediaType> activeType;
        if (SUCCEEDED(reader_->GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM, &activeType)) && activeType) {
            UINT32 w = 0, h = 0;
            if (SUCCEEDED(MFGetAttributeSize(activeType.Get(), MF_MT_FRAME_SIZE, &w, &h)) && w > 0 && h > 0) {
                chosenW = w;
                chosenH = h;
            }
            UINT32 rawStride = 0;
            if (SUCCEEDED(activeType->GetUINT32(MF_MT_DEFAULT_STRIDE, &rawStride)) && rawStride != 0) {
                chosenStride = static_cast<int32_t>(rawStride);
            } else {
                chosenStride = static_cast<int32_t>(chosenW * 4);
            }
        }

        width_ = chosenW;
        height_ = chosenH;
        stride_ = chosenStride;
        return true;
    }

    bool ReadNextFrame(uint8_t** outBgraPtr, size_t* outBgraLen, uint32_t* outW, uint32_t* outH, uint64_t* outSeq) {
        if (!reader_) return false;
        std::lock_guard<std::mutex> lock(mutex_);

        for (int attempt = 0; attempt < 6; ++attempt) {
            DWORD streamIdx = 0;
            DWORD flags = 0;
            LONGLONG ts = 0;
            ComPtr<IMFSample> sample;
            HRESULT hr = reader_->ReadSample(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM,
                0,
                &streamIdx,
                &flags,
                &ts,
                &sample
            );
            if (FAILED(hr)) return false;
            if (!sample) continue;

            ComPtr<IMFMediaBuffer> buffer;
            if (FAILED(sample->ConvertToContiguousBuffer(&buffer)) || !buffer) continue;

            BYTE* data = nullptr;
            DWORD curLen = 0;
            if (FAILED(buffer->Lock(&data, nullptr, &curLen)) || !data) continue;

            const uint32_t w = width_;
            const uint32_t h = height_;
            const size_t tightRow = static_cast<size_t>(w) * 4;
            const size_t totalBytes = tightRow * h;
            const int32_t absStride = stride_ < 0 ? -stride_ : stride_;
            if (curLen < static_cast<DWORD>(absStride) * h) {
                buffer->Unlock();
                continue;
            }

            uint8_t* dst = static_cast<uint8_t*>(std::malloc(totalBytes));
            if (!dst) {
                buffer->Unlock();
                return false;
            }

            for (uint32_t row = 0; row < h; ++row) {
                const uint32_t srcRowIdx = (stride_ < 0) ? (h - 1 - row) : row;
                const uint8_t* srcRow = data + static_cast<size_t>(srcRowIdx) * absStride;
                uint8_t* dstRow = dst + static_cast<size_t>(row) * tightRow;
                std::memcpy(dstRow, srcRow, tightRow);
                for (uint32_t px = 0; px < w; ++px) {
                    dstRow[px * 4 + 3] = 255;
                }
            }
            buffer->Unlock();

            seq_ += 1;
            *outBgraPtr = dst;
            *outBgraLen = totalBytes;
            *outW = w;
            *outH = h;
            *outSeq = seq_;
            return true;
        }
        return false;
    }

    void Stop() {
        std::lock_guard<std::mutex> lock(mutex_);
        reader_.Reset();
        if (mediaSource_) {
            mediaSource_->Shutdown();
            mediaSource_.Reset();
        }
    }

private:
    std::mutex mutex_;
    ComPtr<IMFMediaSource> mediaSource_;
    ComPtr<IMFSourceReader> reader_;
    uint32_t width_ = 1920;
    uint32_t height_ = 1080;
    int32_t stride_ = 1920 * 4;
    uint64_t seq_ = 0;
};

} // namespace truongstream

extern "C" {

char* ts_win_hdmi_list_devices() {
    truongstream::EnsureMfInitialized();
    Microsoft::WRL::ComPtr<IMFAttributes> attrs;
    if (FAILED(MFCreateAttributes(&attrs, 1))) return nullptr;
    attrs->SetGUID(
        MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
        MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID
    );

    IMFActivate** devices = nullptr;
    UINT32 count = 0;
    if (FAILED(MFEnumDeviceSources(attrs.Get(), &devices, &count)) || !devices) {
        return _strdup("");
    }

    std::string lines;
    for (UINT32 i = 0; i < count; ++i) {
        WCHAR* nameW = nullptr;
        WCHAR* linkW = nullptr;
        UINT32 nameLen = 0, linkLen = 0;
        devices[i]->GetAllocatedString(MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &nameW, &nameLen);
        devices[i]->GetAllocatedString(MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, &linkW, &linkLen);

        std::string name = truongstream::WideToUtf8(nameW);
        std::string link = truongstream::WideToUtf8(linkW);
        if (nameW) CoTaskMemFree(nameW);
        if (linkW) CoTaskMemFree(linkW);

        if (!link.empty() && !name.empty() && !truongstream::IsIgnoredVirtualDevice(name)) {
            uint32_t maxW = 1920, maxH = 1080, maxFps = 60;
            truongstream::InspectMaxCapabilities(devices[i], maxW, maxH, maxFps);
            for (char& ch : name) {
                if (ch == '\t' || ch == '\n' || ch == '\r') ch = ' ';
            }
            lines += link + "\t" + name + "\t" + std::to_string(maxW) + "\t" +
                     std::to_string(maxH) + "\t" + std::to_string(maxFps) + "\n";
        }
        devices[i]->Release();
    }
    CoTaskMemFree(devices);
    return _strdup(lines.c_str());
}

void* ts_win_hdmi_open(const char* deviceId, uint32_t width, uint32_t height, uint32_t fps) {
    auto* session = new truongstream::MfHdmiCaptureSession();
    if (!session->Open(deviceId, width, height, fps)) {
        delete session;
        return nullptr;
    }
    return session;
}

bool ts_win_hdmi_set_format(void* ptr, uint32_t width, uint32_t height, uint32_t fps) {
    if (!ptr) return false;
    return static_cast<truongstream::MfHdmiCaptureSession*>(ptr)->ConfigureFormat(width, height, fps);
}

bool ts_win_hdmi_read_frame(
    void* ptr,
    uint8_t** outBgraPtr,
    size_t* outBgraLen,
    uint32_t* outWidth,
    uint32_t* outHeight,
    uint64_t* outSeq
) {
    if (!ptr) return false;
    return static_cast<truongstream::MfHdmiCaptureSession*>(ptr)->ReadNextFrame(
        outBgraPtr, outBgraLen, outWidth, outHeight, outSeq);
}

void ts_win_hdmi_close(void* ptr) {
    if (ptr) {
        auto* session = static_cast<truongstream::MfHdmiCaptureSession*>(ptr);
        session->Stop();
        delete session;
    }
}

void ts_win_free_cstr(char* ptr) {
    if (ptr) std::free(ptr);
}

} // extern "C"
#endif
