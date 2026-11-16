import CoreAudio
import CoreMedia
import CoreMediaIO
import CoreVideo
import Foundation
import IOSurface

private let cmioBridgePath = "/tmp/truongstream-cmio-bridge.bin"
private let cmioBridgeMagic: UInt32 = 0x5453_434D

/// Shared-memory IPC reader connecting the TruongStream app process to the sandboxed CMIOExtension.
private final class CmioSurfaceBridge {
    private var lastSequence: UInt64 = 0

    func pollNextSurface(activeConsumerCount: UInt32) -> (surface: IOSurfaceRef, width: Int32, height: Int32)? {
        let fd = open(cmioBridgePath, O_RDWR)
        guard fd >= 0 else { return nil }
        defer { close(fd) }

        var buf = [UInt8](repeating: 0, count: 64)
        let n = pread(fd, &buf, 64, 0)
        guard n >= 48 else { return nil }

        let magic = buf.withUnsafeBytes { $0.load(fromByteOffset: 0, as: UInt32.self) }
        guard magic == cmioBridgeMagic else { return nil }

        // Report active consumer count back to the host app for auto-sleep sensor protection.
        var countLE = activeConsumerCount.littleEndian
        withUnsafeBytes(of: &countLE) { raw in
            _ = pwrite(fd, raw.baseAddress, 4, 12)
        }

        let active = buf.withUnsafeBytes { $0.load(fromByteOffset: 8, as: UInt32.self) }
        let width = buf.withUnsafeBytes { $0.load(fromByteOffset: 16, as: UInt32.self) }
        let height = buf.withUnsafeBytes { $0.load(fromByteOffset: 20, as: UInt32.self) }
        let surfaceId = buf.withUnsafeBytes { $0.load(fromByteOffset: 28, as: UInt32.self) }
        let sequence = buf.withUnsafeBytes { $0.load(fromByteOffset: 32, as: UInt64.self) }

        guard active != 0, surfaceId != 0, sequence > lastSequence else {
            return nil
        }
        lastSequence = sequence

        guard let surface = IOSurfaceLookup(IOSurfaceID(surfaceId)) else {
            return nil
        }
        return (surface, Int32(width), Int32(height))
    }
}

/// CoreMediaIO System Extension exposing "TruongStream Virtual Camera" to Zoom, OBS, Chrome, and FaceTime.
public final class TruongStreamProviderSource: NSObject, CMIOExtensionProviderSource {
    public private(set) var provider: CMIOExtensionProvider!
    private var deviceSource: TruongStreamDeviceSource!

    public init(clientQueue: DispatchQueue?) {
        super.init()
        self.provider = CMIOExtensionProvider(source: self, clientQueue: clientQueue)
        self.deviceSource = TruongStreamDeviceSource(localizedName: "TruongStream Virtual Camera")
        do {
            try provider.addDevice(deviceSource.device)
        } catch {
            NSLog("TruongStream CMIOExtension addDevice failed: \(error)")
        }
    }

    public var availableProperties: Set<CMIOExtensionProperty> {
        [.providerManufacturer]
    }

    public func providerProperties(forProperties properties: Set<CMIOExtensionProperty>) throws -> CMIOExtensionProviderProperties {
        let props = CMIOExtensionProviderProperties(dictionary: [:])
        if properties.contains(.providerManufacturer) {
            props.manufacturer = "Ian Truong"
        }
        return props
    }

    public func setProviderProperties(_ providerProperties: CMIOExtensionProviderProperties) throws {}

    public func connect(to client: CMIOExtensionClient) throws {}

    public func disconnect(from client: CMIOExtensionClient) {}

    public func pushSharedSurface(_ surface: IOSurfaceRef, width: Int32, height: Int32) {
        deviceSource.pushSurface(surface, width: width, height: height)
    }

    public var activeConsumerCount: Int {
        deviceSource.activeConsumerCount
    }
}

public final class TruongStreamDeviceSource: NSObject, CMIOExtensionDeviceSource {
    public private(set) var device: CMIOExtensionDevice!
    private var streamSource: TruongStreamStreamSource!

    public init(localizedName: String) {
        super.init()
        let deviceID = UUID(uuidString: "A5A8E950-5E6C-4C3A-A147-8B7C3B101001")!
        self.device = CMIOExtensionDevice(
            localizedName: localizedName,
            deviceID: deviceID,
            legacyDeviceID: nil,
            source: self
        )
        self.streamSource = TruongStreamStreamSource(localizedName: "TruongStream Video")
        do {
            try device.addStream(streamSource.stream)
        } catch {
            NSLog("TruongStream CMIOExtension addStream failed: \(error)")
        }
    }

    public var availableProperties: Set<CMIOExtensionProperty> {
        [.deviceTransportType, .deviceModel]
    }

    public func deviceProperties(forProperties properties: Set<CMIOExtensionProperty>) throws -> CMIOExtensionDeviceProperties {
        let props = CMIOExtensionDeviceProperties(dictionary: [:])
        if properties.contains(.deviceTransportType) {
            props.transportType = Int(kAudioDeviceTransportTypeVirtual)
        }
        if properties.contains(.deviceModel) {
            props.model = "TruongStream USB Camera Bridge"
        }
        return props
    }

    public func setDeviceProperties(_ deviceProperties: CMIOExtensionDeviceProperties) throws {}

    public func pushSurface(_ surface: IOSurfaceRef, width: Int32, height: Int32) {
        streamSource.enqueueSurface(surface, width: width, height: height)
    }

    public var activeConsumerCount: Int {
        streamSource.activeConsumers
    }
}

public final class TruongStreamStreamSource: NSObject, CMIOExtensionStreamSource {
    public private(set) var stream: CMIOExtensionStream!
    private var streamingClients = Set<UUID>()
    private let lock = NSLock()
    private var sequenceNumber: UInt64 = 0
    private let bridge = CmioSurfaceBridge()
    private let timerQueue = DispatchQueue(label: "com.iantruong.truongstream.cmio-poll")
    private var timer: DispatchSourceTimer?

    public init(localizedName: String) {
        super.init()
        let streamID = UUID(uuidString: "B6B9F120-2E4D-4910-8112-8B7C3B101002")!
        self.stream = CMIOExtensionStream(
            localizedName: localizedName,
            streamID: streamID,
            direction: .source,
            clockType: .hostTime,
            source: self
        )
        startBridgePolling()
    }

    private func startBridgePolling() {
        let source = DispatchSource.makeTimerSource(queue: timerQueue)
        source.schedule(deadline: .now(), repeating: .milliseconds(16), leeway: .milliseconds(2))
        source.setEventHandler { [weak self] in
            guard let self else { return }
            let consumers = UInt32(self.activeConsumers)
            if let next = self.bridge.pollNextSurface(activeConsumerCount: consumers), consumers > 0 {
                self.enqueueSurface(next.surface, width: next.width, height: next.height)
            }
        }
        source.resume()
        self.timer = source
    }

    public var formats: [CMIOExtensionStreamFormat] {
        [
            makeNv12Format(width: 1920, height: 1080, fps: 60),
            makeNv12Format(width: 3840, height: 2160, fps: 30),
        ]
    }

    private func makeNv12Format(width: Int32, height: Int32, fps: Int32) -> CMIOExtensionStreamFormat {
        var fmtDesc: CMFormatDescription?
        CMVideoFormatDescriptionCreate(
            allocator: kCFAllocatorDefault,
            codecType: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            width: width,
            height: height,
            extensions: nil,
            formatDescriptionOut: &fmtDesc
        )
        return CMIOExtensionStreamFormat(
            formatDescription: fmtDesc!,
            maxFrameDuration: CMTime(value: 1, timescale: fps),
            minFrameDuration: CMTime(value: 1, timescale: fps),
            validFrameDurations: nil
        )
    }

    public var availableProperties: Set<CMIOExtensionProperty> {
        [.streamActiveFormatIndex, .streamFrameDuration]
    }

    public func streamProperties(forProperties properties: Set<CMIOExtensionProperty>) throws -> CMIOExtensionStreamProperties {
        let props = CMIOExtensionStreamProperties(dictionary: [:])
        if properties.contains(.streamActiveFormatIndex) {
            props.activeFormatIndex = 0
        }
        if properties.contains(.streamFrameDuration) {
            props.frameDuration = CMTime(value: 1, timescale: 60)
        }
        return props
    }

    public func setStreamProperties(_ streamProperties: CMIOExtensionStreamProperties) throws {}

    public func authorizedToStartStream(for client: CMIOExtensionClient) -> Bool {
        true
    }

    public func startStream() throws {
        lock.lock()
        streamingClients.insert(UUID())
        lock.unlock()
    }

    public func stopStream() throws {
        lock.lock()
        if !streamingClients.isEmpty {
            streamingClients.removeFirst()
        }
        lock.unlock()
    }

    public var activeConsumers: Int {
        lock.lock()
        defer { lock.unlock() }
        return streamingClients.count
    }

    public func enqueueSurface(_ surface: IOSurfaceRef, width: Int32, height: Int32) {
        guard activeConsumers > 0 else { return }

        var pixelBuffer: Unmanaged<CVPixelBuffer>?
        let status = CVPixelBufferCreateWithIOSurface(
            kCFAllocatorDefault,
            surface,
            nil,
            &pixelBuffer
        )
        guard status == kCVReturnSuccess, let pb = pixelBuffer?.takeRetainedValue() else {
            return
        }

        var fmtDesc: CMVideoFormatDescription?
        guard CMVideoFormatDescriptionCreateForImageBuffer(
            allocator: kCFAllocatorDefault,
            imageBuffer: pb,
            formatDescriptionOut: &fmtDesc
        ) == noErr, let fmtDesc else {
            return
        }

        let pts = CMClockGetTime(CMClockGetHostTimeClock())
        var timing = CMSampleTimingInfo(
            duration: CMTime(value: 1, timescale: 60),
            presentationTimeStamp: pts,
            decodeTimeStamp: .invalid
        )
        var sampleBuffer: CMSampleBuffer?
        guard CMSampleBufferCreateForImageBuffer(
            allocator: kCFAllocatorDefault,
            imageBuffer: pb,
            dataReady: true,
            makeDataReadyCallback: nil,
            refcon: nil,
            formatDescription: fmtDesc,
            sampleTiming: &timing,
            sampleBufferOut: &sampleBuffer
        ) == noErr, let sampleBuffer else {
            return
        }

        sequenceNumber += 1
        stream.send(
            sampleBuffer,
            discontinuity: [],
            hostTimeInNanoseconds: UInt64(CMTimeGetSeconds(pts) * 1_000_000_000)
        )
    }
}
