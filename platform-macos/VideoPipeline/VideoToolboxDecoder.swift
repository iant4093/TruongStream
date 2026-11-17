import AVFoundation
import CoreGraphics
import CoreImage
import CoreMedia
import CoreVideo
import Foundation
import ImageIO
import IOSurface
import Metal
import SystemExtensions
import VideoToolbox

public struct LutRenderParams {
    public var intensity: Float
    public var mirrorHorizontal: Bool
    public var flipVertical: Bool
    public var cropZoom: Float
    public var domainMin: SIMD3<Float>
    public var domainMax: SIMD3<Float>

    public init(
        intensity: Float = 1.0,
        mirrorHorizontal: Bool = false,
        flipVertical: Bool = false,
        cropZoom: Float = 1.0,
        domainMin: SIMD3<Float> = .zero,
        domainMax: SIMD3<Float> = .one
    ) {
        self.intensity = intensity
        self.mirrorHorizontal = mirrorHorizontal
        self.flipVertical = flipVertical
        self.cropZoom = cropZoom
        self.domainMin = domainMin
        self.domainMax = domainMax
    }
}

private struct MetalLutUniforms {
    var intensity: Float
    var mirrorH: UInt32
    var flipV: UInt32
    var cropZoom: Float
    var domainMin: SIMD3<Float>
    var domainMax: SIMD3<Float>
    var outputWidth: UInt32
    var outputHeight: UInt32
}

/// Hardware VideoToolbox MJPEG decoder + single-pass Metal 3D LUT + NV12 IOSurface pipeline.
public final class VideoToolboxLutPipeline {
    private let device: MTLDevice
    private let commandQueue: MTLCommandQueue
    private let computePipeline: MTLComputePipelineState
    private let ciContext: CIContext
    private var textureCache: CVMetalTextureCache?
    private var lutTexture: MTLTexture?
    private var decompressionSession: VTDecompressionSession?
    private var lastFormatDesc: CMVideoFormatDescription?

    // Triple-buffered reusable IOSurface-backed NV12 buffers so exported IOSurfaceIDs
    // remain valid across frames with zero per-frame VRAM allocation.
    private var nv12Pool: [CVPixelBuffer] = []
    private var nv12PoolDims: (Int, Int) = (0, 0)
    private var nv12PoolIndex: Int = 0
    private var fallbackBgraBuffer: CVPixelBuffer?
    private var fallbackBgraDims: (Int, Int) = (0, 0)
    private var previewTexture: MTLTexture?
    private var previewTextureDims: (Int, Int) = (0, 0)

    public init(metalShaderSource: String) throws {
        guard let device = MTLCreateSystemDefaultDevice(),
              let queue = device.makeCommandQueue() else {
            throw NSError(domain: "TruongStream.Metal", code: -1, userInfo: [
                NSLocalizedDescriptionKey: "No Metal device available"
            ])
        }
        self.device = device
        self.commandQueue = queue
        self.ciContext = CIContext(mtlDevice: device, options: [.useSoftwareRenderer: false])

        let library = try device.makeLibrary(source: metalShaderSource, options: nil)
        guard let fn = library.makeFunction(name: "apply_lut_and_convert_nv12") else {
            throw NSError(domain: "TruongStream.Metal", code: -2, userInfo: [
                NSLocalizedDescriptionKey: "Missing apply_lut_and_convert_nv12 kernel"
            ])
        }
        self.computePipeline = try device.makeComputePipelineState(function: fn)

        var cache: CVMetalTextureCache?
        CVMetalTextureCacheCreate(kCFAllocatorDefault, nil, device, nil, &cache)
        self.textureCache = cache
    }

    public func uploadLut3D(size: Int, rgba32FloatData: UnsafeRawPointer) {
        let desc = MTLTextureDescriptor()
        desc.textureType = .type3D
        desc.pixelFormat = .rgba32Float
        desc.width = size
        desc.height = size
        desc.depth = size
        desc.usage = [.shaderRead]
        desc.storageMode = .shared

        guard let tex = device.makeTexture(descriptor: desc) else { return }
        let bytesPerRow = size * MemoryLayout<SIMD4<Float>>.stride
        let bytesPerImage = bytesPerRow * size
        tex.replace(
            region: MTLRegionMake3D(0, 0, 0, size, size, size),
            mipmapLevel: 0,
            slice: 0,
            withBytes: rgba32FloatData,
            bytesPerRow: bytesPerRow,
            bytesPerImage: bytesPerImage
        )
        self.lutTexture = tex
    }

    public func decodeJpegToPixelBuffer(_ jpegData: Data, width: Int32, height: Int32) -> CVPixelBuffer? {
        if let vtBuf = decodeWithVideoToolbox(jpegData, width: width, height: height) {
            return vtBuf
        }
        return decodeWithImageIOFallback(jpegData)
    }

    private func decodeWithVideoToolbox(_ jpegData: Data, width: Int32, height: Int32) -> CVPixelBuffer? {
        guard width > 0, height > 0 else { return nil }
        var formatDesc: CMVideoFormatDescription?
        let status = CMVideoFormatDescriptionCreate(
            allocator: kCFAllocatorDefault,
            codecType: kCMVideoCodecType_JPEG,
            width: width,
            height: height,
            extensions: nil,
            formatDescriptionOut: &formatDesc
        )
        guard status == noErr, let formatDesc else { return nil }

        if decompressionSession == nil || lastFormatDesc == nil ||
            !CMFormatDescriptionEqual(lastFormatDesc!, otherFormatDescription: formatDesc) {
            if let old = decompressionSession {
                VTDecompressionSessionInvalidate(old)
            }
            let attrs: [CFString: Any] = [
                kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA,
                kCVPixelBufferMetalCompatibilityKey: true,
                kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
            ]
            var session: VTDecompressionSession?
            let err = VTDecompressionSessionCreate(
                allocator: kCFAllocatorDefault,
                formatDescription: formatDesc,
                decoderSpecification: nil,
                imageBufferAttributes: attrs as CFDictionary,
                outputCallback: nil,
                decompressionSessionOut: &session
            )
            guard err == noErr else { return nil }
            decompressionSession = session
            lastFormatDesc = formatDesc
        }

        guard let session = decompressionSession else { return nil }

        var blockBuffer: CMBlockBuffer?
        let blockStatus = jpegData.withUnsafeBytes { rawBuf -> OSStatus in
            guard let base = rawBuf.baseAddress else { return -1 }
            return CMBlockBufferCreateWithMemoryBlock(
                allocator: kCFAllocatorDefault,
                memoryBlock: nil,
                blockLength: jpegData.count,
                blockAllocator: kCFAllocatorDefault,
                customBlockSource: nil,
                offsetToData: 0,
                dataLength: jpegData.count,
                flags: 0,
                blockBufferOut: &blockBuffer
            ).flatMapIfSuccess {
                CMBlockBufferReplaceDataBytes(
                    with: base,
                    blockBuffer: blockBuffer!,
                    offsetIntoDestination: 0,
                    dataLength: jpegData.count
                )
            }
        }
        guard blockStatus == noErr, let blockBuffer else { return nil }

        var sampleBuffer: CMSampleBuffer?
        var sampleSize = jpegData.count
        guard CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault,
            dataBuffer: blockBuffer,
            formatDescription: formatDesc,
            sampleCount: 1,
            sampleTimingEntryCount: 0,
            sampleTimingArray: nil,
            sampleSizeEntryCount: 1,
            sampleSizeArray: &sampleSize,
            sampleBufferOut: &sampleBuffer
        ) == noErr, let sampleBuffer else {
            return nil
        }

        var decodedBuffer: CVPixelBuffer?
        var infoFlags = VTDecodeInfoFlags()
        let decodeStatus = VTDecompressionSessionDecodeFrame(
            session,
            sampleBuffer: sampleBuffer,
            flags: [],
            infoFlagsOut: &infoFlags
        ) { _, _, imageBuffer, _, _ in
            decodedBuffer = imageBuffer
        }
        VTDecompressionSessionWaitForAsynchronousFrames(session)
        guard decodeStatus == noErr else { return nil }
        return decodedBuffer
    }

    private func decodeWithImageIOFallback(_ jpegData: Data) -> CVPixelBuffer? {
        guard let source = CGImageSourceCreateWithData(jpegData as CFData, nil),
              let cgImage = CGImageSourceCreateImageAtIndex(source, 0, nil) else {
            return nil
        }
        let w = cgImage.width
        let h = cgImage.height
        guard w > 0, h > 0 else { return nil }

        if fallbackBgraBuffer == nil || fallbackBgraDims != (w, h) {
            let attrs: [CFString: Any] = [
                kCVPixelBufferMetalCompatibilityKey: true,
                kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
            ]
            var pb: CVPixelBuffer?
            guard CVPixelBufferCreate(
                kCFAllocatorDefault,
                w,
                h,
                kCVPixelFormatType_32BGRA,
                attrs as CFDictionary,
                &pb
            ) == kCVReturnSuccess, let pb else {
                return nil
            }
            fallbackBgraBuffer = pb
            fallbackBgraDims = (w, h)
        }

        guard let pb = fallbackBgraBuffer else { return nil }
        CVPixelBufferLockBaseAddress(pb, [])
        defer { CVPixelBufferUnlockBaseAddress(pb, []) }

        if let base = CVPixelBufferGetBaseAddress(pb) {
            let bytesPerRow = CVPixelBufferGetBytesPerRow(pb)
            let cs = CGColorSpaceCreateDeviceRGB()
            let bitmapInfo = CGBitmapInfo.byteOrder32Little.rawValue |
                CGImageAlphaInfo.premultipliedFirst.rawValue
            if let ctx = CGContext(
                data: base,
                width: w,
                height: h,
                bitsPerComponent: 8,
                bytesPerRow: bytesPerRow,
                space: cs,
                bitmapInfo: bitmapInfo
            ) {
                ctx.draw(cgImage, in: CGRect(x: 0, y: 0, width: w, height: h))
                return pb
            }
        }
        return nil
    }

    public func createBgraPixelBuffer(
        from bgraPtr: UnsafePointer<UInt8>,
        length: Int,
        width: Int,
        height: Int
    ) -> CVPixelBuffer? {
        guard width >= 2, height >= 2, length >= width * height * 4 else { return nil }
        if fallbackBgraBuffer == nil || fallbackBgraDims != (width, height) {
            let attrs: [CFString: Any] = [
                kCVPixelBufferMetalCompatibilityKey: true,
                kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
            ]
            var pb: CVPixelBuffer?
            guard CVPixelBufferCreate(
                kCFAllocatorDefault,
                width,
                height,
                kCVPixelFormatType_32BGRA,
                attrs as CFDictionary,
                &pb
            ) == kCVReturnSuccess, let pb else {
                return nil
            }
            fallbackBgraBuffer = pb
            fallbackBgraDims = (width, height)
        }
        guard let pb = fallbackBgraBuffer else { return nil }
        CVPixelBufferLockBaseAddress(pb, [])
        defer { CVPixelBufferUnlockBaseAddress(pb, []) }
        guard let dstBase = CVPixelBufferGetBaseAddress(pb) else { return nil }

        let dstStride = CVPixelBufferGetBytesPerRow(pb)
        let srcStride = width * 4
        if dstStride == srcStride {
            dstBase.copyMemory(from: bgraPtr, byteCount: srcStride * height)
        } else {
            let copyBytes = min(dstStride, srcStride)
            for row in 0..<height {
                dstBase.advanced(by: row * dstStride).copyMemory(
                    from: bgraPtr.advanced(by: row * srcStride),
                    byteCount: copyBytes
                )
            }
        }
        return pb
    }

    private func nextPooledNv12Buffer(width: Int, height: Int) -> CVPixelBuffer? {
        if nv12Pool.count != 3 || nv12PoolDims != (width, height) {
            nv12Pool.removeAll()
            let nv12Attrs: [CFString: Any] = [
                kCVPixelBufferMetalCompatibilityKey: true,
                kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
            ]
            for _ in 0..<3 {
                var pb: CVPixelBuffer?
                guard CVPixelBufferCreate(
                    kCFAllocatorDefault,
                    width,
                    height,
                    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                    nv12Attrs as CFDictionary,
                    &pb
                ) == kCVReturnSuccess, let pb else {
                    return nil
                }
                nv12Pool.append(pb)
            }
            nv12PoolDims = (width, height)
            nv12PoolIndex = 0
        }
        let buf = nv12Pool[nv12PoolIndex]
        nv12PoolIndex = (nv12PoolIndex + 1) % nv12Pool.count
        return buf
    }

    private func ensurePreviewTexture(width: Int, height: Int) -> MTLTexture? {
        if previewTexture == nil || previewTextureDims != (width, height) {
            let previewDesc = MTLTextureDescriptor.texture2DDescriptor(
                pixelFormat: .bgra8Unorm,
                width: width,
                height: height,
                mipmapped: false
            )
            previewDesc.usage = [.shaderWrite, .shaderRead]
            previewDesc.storageMode = .shared
            previewTexture = device.makeTexture(descriptor: previewDesc)
            previewTextureDims = (width, height)
        }
        return previewTexture
    }

    public func processToSharedNv12(
        sourceBuffer: CVPixelBuffer,
        outputWidth: Int,
        outputHeight: Int,
        params: LutRenderParams
    ) -> (pixelBuffer: CVPixelBuffer, previewTexture: MTLTexture)? {
        guard let cache = textureCache, let lutTex = lutTexture else { return nil }
        let srcWidth = CVPixelBufferGetWidth(sourceBuffer)
        let srcHeight = CVPixelBufferGetHeight(sourceBuffer)

        var cvSrcTex: CVMetalTexture?
        guard CVMetalTextureCacheCreateTextureFromImage(
            kCFAllocatorDefault,
            cache,
            sourceBuffer,
            nil,
            .bgra8Unorm,
            srcWidth,
            srcHeight,
            0,
            &cvSrcTex
        ) == kCVReturnSuccess,
              let cvSrcTex,
              let srcTex = CVMetalTextureGetTexture(cvSrcTex),
              let nv12Out = nextPooledNv12Buffer(width: outputWidth, height: outputHeight) else {
            return nil
        }

        var cvY: CVMetalTexture?
        var cvUV: CVMetalTexture?
        guard CVMetalTextureCacheCreateTextureFromImage(
            kCFAllocatorDefault, cache, nv12Out, nil, .r8Unorm, outputWidth, outputHeight, 0, &cvY
        ) == kCVReturnSuccess,
              CVMetalTextureCacheCreateTextureFromImage(
                  kCFAllocatorDefault, cache, nv12Out, nil, .rg8Unorm, max(1, outputWidth / 2), max(1, outputHeight / 2), 1, &cvUV
              ) == kCVReturnSuccess,
              let yTex = cvY.flatMap({ CVMetalTextureGetTexture($0) }),
              let uvTex = cvUV.flatMap({ CVMetalTextureGetTexture($0) }),
              let previewTex = ensurePreviewTexture(width: outputWidth, height: outputHeight),
              let cmdBuf = commandQueue.makeCommandBuffer(),
              let encoder = cmdBuf.makeComputeCommandEncoder() else {
            return nil
        }

        var uniforms = MetalLutUniforms(
            intensity: params.intensity,
            mirrorH: params.mirrorHorizontal ? 1 : 0,
            flipV: params.flipVertical ? 1 : 0,
            cropZoom: params.cropZoom,
            domainMin: params.domainMin,
            domainMax: params.domainMax,
            outputWidth: UInt32(outputWidth),
            outputHeight: UInt32(outputHeight)
        )

        encoder.setComputePipelineState(computePipeline)
        encoder.setTexture(srcTex, index: 0)
        encoder.setTexture(lutTex, index: 1)
        encoder.setTexture(previewTex, index: 2)
        encoder.setTexture(yTex, index: 3)
        encoder.setTexture(uvTex, index: 4)
        encoder.setBytes(&uniforms, length: MemoryLayout<MetalLutUniforms>.stride, index: 0)

        let tgSize = MTLSize(width: 16, height: 16, depth: 1)
        let gridSize = MTLSize(
            width: (outputWidth + 15) / 16 * 16,
            height: (outputHeight + 15) / 16 * 16,
            depth: 1
        )
        encoder.dispatchThreads(gridSize, threadsPerThreadgroup: tgSize)
        encoder.endEncoding()
        cmdBuf.commit()
        cmdBuf.waitUntilCompleted()

        return (nv12Out, previewTex)
    }

    fileprivate func encodePreviewJpeg(_ texture: MTLTexture) -> Data? {
        let cs = CGColorSpaceCreateDeviceRGB()
        guard let rawCi = CIImage(mtlTexture: texture, options: [.colorSpace: cs]) else {
            return nil
        }
        // Metal texture origin is top-left while CIImage assumes bottom-left.
        var img = rawCi
            .transformed(by: CGAffineTransform(scaleX: 1, y: -1))
            .transformed(by: CGAffineTransform(translationX: 0, y: CGFloat(texture.height)))
        if texture.width > 1920 {
            let previewScale = 1920.0 / CGFloat(texture.width)
            img = img.transformed(by: CGAffineTransform(scaleX: previewScale, y: previewScale))
        }
        return ciContext.jpegRepresentation(
            of: img,
            colorSpace: cs,
            options: [kCGImageDestinationLossyCompressionQuality as CIImageRepresentationOption: 0.85]
        )
    }
}

private extension OSStatus {
    func flatMapIfSuccess(_ body: () -> OSStatus) -> OSStatus {
        self == noErr ? body() : self
    }
}

// MARK: - C ABI FFI for Rust `truongstream-macos`

@_cdecl("ts_macos_pipeline_create")
public func tsMacosPipelineCreate(_ shaderSourceCStr: UnsafePointer<CChar>?) -> UnsafeMutableRawPointer? {
    guard let shaderSourceCStr else { return nil }
    let source = String(cString: shaderSourceCStr)
    guard let pipeline = try? VideoToolboxLutPipeline(metalShaderSource: source) else {
        return nil
    }
    return Unmanaged.passRetained(pipeline).toOpaque()
}

@_cdecl("ts_macos_pipeline_destroy")
public func tsMacosPipelineDestroy(_ ptr: UnsafeMutableRawPointer?) {
    guard let ptr else { return }
    Unmanaged<VideoToolboxLutPipeline>.fromOpaque(ptr).release()
}

@_cdecl("ts_macos_pipeline_upload_lut")
public func tsMacosPipelineUploadLut(
    _ ptr: UnsafeMutableRawPointer?,
    _ size: Int32,
    _ rgba32FloatPtr: UnsafeRawPointer?
) -> Bool {
    guard let ptr, let rgba32FloatPtr, size >= 2 else { return false }
    let pipeline = Unmanaged<VideoToolboxLutPipeline>.fromOpaque(ptr).takeUnretainedValue()
    pipeline.uploadLut3D(size: Int(size), rgba32FloatData: rgba32FloatPtr)
    return true
}

@_cdecl("ts_macos_free_buffer")
public func tsMacosFreeBuffer(_ ptr: UnsafeMutablePointer<UInt8>?) {
    guard let ptr else { return }
    free(ptr)
}

private func allocCopy(_ data: Data, outPtr: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?, outLen: UnsafeMutablePointer<Int>?) {
    guard let outPtr, let outLen else { return }
    if data.isEmpty {
        outPtr.pointee = nil
        outLen.pointee = 0
        return
    }
    let buf = malloc(data.count)!.assumingMemoryBound(to: UInt8.self)
    data.copyBytes(to: buf, count: data.count)
    outPtr.pointee = buf
    outLen.pointee = data.count
}

private func runPipelineExport(
    pipeline: VideoToolboxLutPipeline,
    srcBuffer: CVPixelBuffer,
    startNs: UInt64,
    targetWidth: UInt32,
    targetHeight: UInt32,
    intensity: Float,
    mirrorH: Bool,
    flipV: Bool,
    cropZoom: Float,
    domMinR: Float,
    domMinG: Float,
    domMinB: Float,
    domMaxR: Float,
    domMaxG: Float,
    domMaxB: Float,
    wantNv12Bytes: Bool,
    wantPreviewJpeg: Bool,
    outWidth: UnsafeMutablePointer<UInt32>?,
    outHeight: UnsafeMutablePointer<UInt32>?,
    outIoSurfaceId: UnsafeMutablePointer<UInt32>?,
    outGpuMs: UnsafeMutablePointer<Float>?,
    outYPtr: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    outYLen: UnsafeMutablePointer<Int>?,
    outUvPtr: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    outUvLen: UnsafeMutablePointer<Int>?,
    outPreviewPtr: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    outPreviewLen: UnsafeMutablePointer<Int>?
) -> Bool {
    let srcW = CVPixelBufferGetWidth(srcBuffer)
    let srcH = CVPixelBufferGetHeight(srcBuffer)
    let outW = max(2, ((targetWidth > 0 && targetHeight > 0) ? Int(targetWidth) : srcW) & ~1)
    let outH = max(2, ((targetWidth > 0 && targetHeight > 0) ? Int(targetHeight) : srcH) & ~1)
    let params = LutRenderParams(
        intensity: intensity,
        mirrorHorizontal: mirrorH,
        flipVertical: flipV,
        cropZoom: cropZoom,
        domainMin: SIMD3<Float>(domMinR, domMinG, domMinB),
        domainMax: SIMD3<Float>(domMaxR, domMaxG, domMaxB)
    )

    guard let result = pipeline.processToSharedNv12(
        sourceBuffer: srcBuffer,
        outputWidth: outW,
        outputHeight: outH,
        params: params
    ) else {
        return false
    }

    let nv12 = result.pixelBuffer
    outWidth?.pointee = UInt32(outW)
    outHeight?.pointee = UInt32(outH)
    if let surface = CVPixelBufferGetIOSurface(nv12)?.takeUnretainedValue() {
        outIoSurfaceId?.pointee = IOSurfaceGetID(surface)
    } else {
        outIoSurfaceId?.pointee = 0
    }

    if wantNv12Bytes {
        CVPixelBufferLockBaseAddress(nv12, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(nv12, .readOnly) }

        if let yBase = CVPixelBufferGetBaseAddressOfPlane(nv12, 0) {
            let yStride = CVPixelBufferGetBytesPerRowOfPlane(nv12, 0)
            var yData = Data(count: outW * outH)
            yData.withUnsafeMutableBytes { dst in
                for row in 0..<outH {
                    let srcRow = yBase.advanced(by: row * yStride)
                    dst.baseAddress!.advanced(by: row * outW).copyMemory(from: srcRow, byteCount: outW)
                }
            }
            allocCopy(yData, outPtr: outYPtr, outLen: outYLen)
        }

        if let uvBase = CVPixelBufferGetBaseAddressOfPlane(nv12, 1) {
            let uvStride = CVPixelBufferGetBytesPerRowOfPlane(nv12, 1)
            let uvRows = outH / 2
            let uvRowBytes = (outW / 2) * 2
            var uvData = Data(count: uvRows * uvRowBytes)
            uvData.withUnsafeMutableBytes { dst in
                for row in 0..<uvRows {
                    let srcRow = uvBase.advanced(by: row * uvStride)
                    dst.baseAddress!.advanced(by: row * uvRowBytes).copyMemory(from: srcRow, byteCount: uvRowBytes)
                }
            }
            allocCopy(uvData, outPtr: outUvPtr, outLen: outUvLen)
        }
    }

    if wantPreviewJpeg, let jpegOut = pipeline.encodePreviewJpeg(result.previewTexture) {
        allocCopy(jpegOut, outPtr: outPreviewPtr, outLen: outPreviewLen)
    }

    let elapsedMs = Float(DispatchTime.now().uptimeNanoseconds - startNs) / 1_000_000.0
    outGpuMs?.pointee = elapsedMs
    return true
}

@_cdecl("ts_macos_pipeline_process_jpeg")
public func tsMacosPipelineProcessJpeg(
    _ ptr: UnsafeMutableRawPointer?,
    _ jpegPtr: UnsafePointer<UInt8>?,
    _ jpegLen: Int,
    _ widthHint: UInt32,
    _ heightHint: UInt32,
    _ targetWidth: UInt32,
    _ targetHeight: UInt32,
    _ intensity: Float,
    _ mirrorH: Bool,
    _ flipV: Bool,
    _ cropZoom: Float,
    _ domMinR: Float,
    _ domMinG: Float,
    _ domMinB: Float,
    _ domMaxR: Float,
    _ domMaxG: Float,
    _ domMaxB: Float,
    _ wantNv12Bytes: Bool,
    _ wantPreviewJpeg: Bool,
    _ outWidth: UnsafeMutablePointer<UInt32>?,
    _ outHeight: UnsafeMutablePointer<UInt32>?,
    _ outIoSurfaceId: UnsafeMutablePointer<UInt32>?,
    _ outGpuMs: UnsafeMutablePointer<Float>?,
    _ outYPtr: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    _ outYLen: UnsafeMutablePointer<Int>?,
    _ outUvPtr: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    _ outUvLen: UnsafeMutablePointer<Int>?,
    _ outPreviewPtr: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    _ outPreviewLen: UnsafeMutablePointer<Int>?
) -> Bool {
    guard let ptr, let jpegPtr, jpegLen > 0 else { return false }
    let pipeline = Unmanaged<VideoToolboxLutPipeline>.fromOpaque(ptr).takeUnretainedValue()
    let start = DispatchTime.now().uptimeNanoseconds

    let jpegData = Data(bytes: jpegPtr, count: jpegLen)
    guard let srcBuffer = pipeline.decodeJpegToPixelBuffer(
        jpegData,
        width: Int32(widthHint),
        height: Int32(heightHint)
    ) else {
        return false
    }

    return runPipelineExport(
        pipeline: pipeline,
        srcBuffer: srcBuffer,
        startNs: start,
        targetWidth: targetWidth,
        targetHeight: targetHeight,
        intensity: intensity,
        mirrorH: mirrorH,
        flipV: flipV,
        cropZoom: cropZoom,
        domMinR: domMinR,
        domMinG: domMinG,
        domMinB: domMinB,
        domMaxR: domMaxR,
        domMaxG: domMaxG,
        domMaxB: domMaxB,
        wantNv12Bytes: wantNv12Bytes,
        wantPreviewJpeg: wantPreviewJpeg,
        outWidth: outWidth,
        outHeight: outHeight,
        outIoSurfaceId: outIoSurfaceId,
        outGpuMs: outGpuMs,
        outYPtr: outYPtr,
        outYLen: outYLen,
        outUvPtr: outUvPtr,
        outUvLen: outUvLen,
        outPreviewPtr: outPreviewPtr,
        outPreviewLen: outPreviewLen
    )
}

@_cdecl("ts_macos_pipeline_process_bgra")
public func tsMacosPipelineProcessBgra(
    _ ptr: UnsafeMutableRawPointer?,
    _ bgraPtr: UnsafePointer<UInt8>?,
    _ bgraLen: Int,
    _ srcWidth: UInt32,
    _ srcHeight: UInt32,
    _ targetWidth: UInt32,
    _ targetHeight: UInt32,
    _ intensity: Float,
    _ mirrorH: Bool,
    _ flipV: Bool,
    _ cropZoom: Float,
    _ domMinR: Float,
    _ domMinG: Float,
    _ domMinB: Float,
    _ domMaxR: Float,
    _ domMaxG: Float,
    _ domMaxB: Float,
    _ wantNv12Bytes: Bool,
    _ wantPreviewJpeg: Bool,
    _ outWidth: UnsafeMutablePointer<UInt32>?,
    _ outHeight: UnsafeMutablePointer<UInt32>?,
    _ outIoSurfaceId: UnsafeMutablePointer<UInt32>?,
    _ outGpuMs: UnsafeMutablePointer<Float>?,
    _ outYPtr: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    _ outYLen: UnsafeMutablePointer<Int>?,
    _ outUvPtr: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    _ outUvLen: UnsafeMutablePointer<Int>?,
    _ outPreviewPtr: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    _ outPreviewLen: UnsafeMutablePointer<Int>?
) -> Bool {
    guard let ptr, let bgraPtr, srcWidth >= 2, srcHeight >= 2 else { return false }
    let pipeline = Unmanaged<VideoToolboxLutPipeline>.fromOpaque(ptr).takeUnretainedValue()
    let start = DispatchTime.now().uptimeNanoseconds

    guard let srcBuffer = pipeline.createBgraPixelBuffer(
        from: bgraPtr,
        length: bgraLen,
        width: Int(srcWidth),
        height: Int(srcHeight)
    ) else {
        return false
    }

    return runPipelineExport(
        pipeline: pipeline,
        srcBuffer: srcBuffer,
        startNs: start,
        targetWidth: targetWidth,
        targetHeight: targetHeight,
        intensity: intensity,
        mirrorH: mirrorH,
        flipV: flipV,
        cropZoom: cropZoom,
        domMinR: domMinR,
        domMinG: domMinG,
        domMinB: domMinB,
        domMaxR: domMaxR,
        domMaxG: domMaxG,
        domMaxB: domMaxB,
        wantNv12Bytes: wantNv12Bytes,
        wantPreviewJpeg: wantPreviewJpeg,
        outWidth: outWidth,
        outHeight: outHeight,
        outIoSurfaceId: outIoSurfaceId,
        outGpuMs: outGpuMs,
        outYPtr: outYPtr,
        outYLen: outYLen,
        outUvPtr: outUvPtr,
        outUvLen: outUvLen,
        outPreviewPtr: outPreviewPtr,
        outPreviewLen: outPreviewLen
    )
}

private final class ExtensionActivationDelegate: NSObject, OSSystemExtensionRequestDelegate {
    static let shared = ExtensionActivationDelegate()

    func request(
        _ request: OSSystemExtensionRequest,
        actionForReplacingExtension existing: OSSystemExtensionProperties,
        withExtension ext: OSSystemExtensionProperties
    ) -> OSSystemExtensionRequest.ReplacementAction {
        .replace
    }

    func requestNeedsUserApproval(_ request: OSSystemExtensionRequest) {}
    func request(_ request: OSSystemExtensionRequest, didFinishWithResult result: OSSystemExtensionRequest.Result) {}
    func request(_ request: OSSystemExtensionRequest, didFailWithError error: Error) {}
}

@_cdecl("ts_macos_activate_camera_extension")
public func ts_macos_activate_camera_extension() -> UnsafeMutablePointer<CChar>? {
    let extPath =
        Bundle.main.bundlePath
        + "/Contents/Library/SystemExtensions/com.iantruong.truongstream.cameraextension.systemextension"
    let message: String
    if FileManager.default.fileExists(atPath: extPath) {
        let request = OSSystemExtensionRequest.activationRequest(
            forExtensionWithIdentifier: "com.iantruong.truongstream.cameraextension",
            queue: .main
        )
        request.delegate = ExtensionActivationDelegate.shared
        OSSystemExtensionManager.shared.submitRequest(request)
        message =
            "Submitted macOS Camera Extension activation request. Approve in System Settings › Privacy & Security › Camera Extensions if prompted."
    } else {
        message =
            "macOS CMIO System Extension requires a signed Apple Developer ID + notarized build. Use the built-in Local MJPEG Stream (http://127.0.0.1:49210/stream.mjpeg) or Clean Output Mode in OBS right now."
    }
    return strdup(message)
}

@_cdecl("ts_macos_free_cstr")
public func ts_macos_free_cstr(_ ptr: UnsafeMutablePointer<CChar>?) {
    if let ptr {
        free(ptr)
    }
}

// MARK: - AVFoundation Uncompressed 1080p / 4K HDMI Capture

private final class MacOsHdmiCaptureSession: NSObject, AVCaptureVideoDataOutputSampleBufferDelegate, @unchecked Sendable {
    private let session = AVCaptureSession()
    private let device: AVCaptureDevice
    private let output = AVCaptureVideoDataOutput()
    private let queue = DispatchQueue(label: "com.iantruong.truongstream.hdmi", qos: .userInteractive)
    private let lock = NSLock()
    private let frameSemaphore = DispatchSemaphore(value: 0)

    private var latestBgra = Data()
    private var latestWidth: UInt32 = 0
    private var latestHeight: UInt32 = 0
    private var latestSeq: UInt64 = 0
    private var lastConsumedSeq: UInt64 = 0
    private var isRunning = false

    init(device: AVCaptureDevice, width: UInt32, height: UInt32, fps: UInt32) throws {
        self.device = device
        super.init()

        session.beginConfiguration()
        let input = try AVCaptureDeviceInput(device: device)
        guard session.canAddInput(input) else {
            session.commitConfiguration()
            throw NSError(domain: "TruongStream.HDMI", code: -1, userInfo: [
                NSLocalizedDescriptionKey: "Unable to attach HDMI capture device input"
            ])
        }
        session.addInput(input)

        output.videoSettings = [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA
        ]
        output.alwaysDiscardsLateVideoFrames = true
        output.setSampleBufferDelegate(self, queue: queue)
        guard session.canAddOutput(output) else {
            session.commitConfiguration()
            throw NSError(domain: "TruongStream.HDMI", code: -2, userInfo: [
                NSLocalizedDescriptionKey: "Unable to attach BGRA video output for HDMI capture"
            ])
        }
        session.addOutput(output)
        session.commitConfiguration()

        try configureFormat(width: width, height: height, fps: fps)
        session.startRunning()
        isRunning = true
    }

    func configureFormat(width: UInt32, height: UInt32, fps: UInt32) throws {
        let targetW = Int32(max(width, 640))
        let targetH = Int32(max(height, 480))
        let targetFps = Double(max(fps, 15))

        var bestFormat: AVCaptureDevice.Format?
        var bestRange: AVFrameRateRange?
        var bestScore = Int.min

        for fmt in device.formats {
            let dims = CMVideoFormatDescriptionGetDimensions(fmt.formatDescription)
            let subType = CMFormatDescriptionGetMediaSubType(fmt.formatDescription)
            let isUncompressed = subType != kCMVideoCodecType_JPEG && subType != kCMVideoCodecType_JPEG_OpenDML

            for range in fmt.videoSupportedFrameRateRanges {
                var score = 0
                if dims.width == targetW && dims.height == targetH {
                    score += 100_000
                } else {
                    let areaDiff = abs(Int(dims.width * dims.height) - Int(targetW * targetH))
                    score -= areaDiff / 1000
                }
                if isUncompressed {
                    score += 5_000
                }
                if range.maxFrameRate >= targetFps {
                    score += 1_000
                } else {
                    score += Int(range.maxFrameRate * 10)
                }
                if score > bestScore {
                    bestScore = score
                    bestFormat = fmt
                    bestRange = range
                }
            }
        }

        guard let chosenFormat = bestFormat, let chosenRange = bestRange else { return }
        try device.lockForConfiguration()
        defer { device.unlockForConfiguration() }
        device.activeFormat = chosenFormat
        let clampedFps = max(chosenRange.minFrameRate, min(targetFps, chosenRange.maxFrameRate))
        let timescale = CMTimeScale(max(1, Int32(clampedFps.rounded())))
        let duration = CMTime(value: 1, timescale: timescale)
        device.activeVideoMinFrameDuration = duration
        device.activeVideoMaxFrameDuration = duration
    }

    func captureOutput(
        _ output: AVCaptureOutput,
        didOutput sampleBuffer: CMSampleBuffer,
        from connection: AVCaptureConnection
    ) {
        guard let pb = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        CVPixelBufferLockBaseAddress(pb, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pb, .readOnly) }

        let w = CVPixelBufferGetWidth(pb)
        let h = CVPixelBufferGetHeight(pb)
        let stride = CVPixelBufferGetBytesPerRow(pb)
        guard w >= 2, h >= 2, let base = CVPixelBufferGetBaseAddress(pb) else { return }

        let tightRowBytes = w * 4
        var packed = Data(count: tightRowBytes * h)
        packed.withUnsafeMutableBytes { dstRaw in
            guard let dstBase = dstRaw.baseAddress else { return }
            if stride == tightRowBytes {
                dstBase.copyMemory(from: base, byteCount: tightRowBytes * h)
            } else {
                for row in 0..<h {
                    dstBase.advanced(by: row * tightRowBytes).copyMemory(
                        from: base.advanced(by: row * stride),
                        byteCount: tightRowBytes
                    )
                }
            }
        }

        lock.lock()
        latestBgra = packed
        latestWidth = UInt32(w)
        latestHeight = UInt32(h)
        latestSeq &+= 1
        lock.unlock()
        frameSemaphore.signal()
    }

    func nextFrame(
        outPtr: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
        outLen: UnsafeMutablePointer<Int>?,
        outWidth: UnsafeMutablePointer<UInt32>?,
        outHeight: UnsafeMutablePointer<UInt32>?,
        outSeq: UnsafeMutablePointer<UInt64>?
    ) -> Bool {
        lock.lock()
        let needWait = latestSeq == lastConsumedSeq
        lock.unlock()
        if needWait {
            _ = frameSemaphore.wait(timeout: .now() + .milliseconds(250))
        }

        lock.lock()
        defer { lock.unlock() }
        guard !latestBgra.isEmpty, latestSeq > 0, latestSeq != lastConsumedSeq else {
            return false
        }
        lastConsumedSeq = latestSeq
        outWidth?.pointee = latestWidth
        outHeight?.pointee = latestHeight
        outSeq?.pointee = latestSeq
        allocCopy(latestBgra, outPtr: outPtr, outLen: outLen)
        return true
    }

    func stop() {
        if isRunning {
            session.stopRunning()
            isRunning = false
        }
    }
}

private func discoverHdmiDevices() -> [AVCaptureDevice] {
    let discovery = AVCaptureDevice.DiscoverySession(
        deviceTypes: [.external],
        mediaType: .video,
        position: .unspecified
    )
    return discovery.devices.filter { dev in
        let lower = dev.localizedName.lowercased()
        return !lower.contains("truongstream") && !lower.contains("obs virtual")
    }
}

private func requestCameraPermissionIfNeeded() {
    let status = AVCaptureDevice.authorizationStatus(for: .video)
    if status == .notDetermined {
        let sem = DispatchSemaphore(value: 0)
        AVCaptureDevice.requestAccess(for: .video) { _ in
            sem.signal()
        }
        _ = sem.wait(timeout: .now() + .seconds(10))
    }
}

@_cdecl("ts_macos_hdmi_list_devices")
public func ts_macos_hdmi_list_devices() -> UnsafeMutablePointer<CChar>? {
    let devices = discoverHdmiDevices()
    var lines = ""
    for dev in devices {
        var maxW: Int32 = 1920
        var maxH: Int32 = 1080
        var maxFps = 60
        for fmt in dev.formats {
            let dims = CMVideoFormatDescriptionGetDimensions(fmt.formatDescription)
            if dims.width * dims.height >= maxW * maxH {
                maxW = dims.width
                maxH = dims.height
                let fmtFps = fmt.videoSupportedFrameRateRanges
                    .map { Int($0.maxFrameRate.rounded()) }
                    .max() ?? 60
                maxFps = max(maxFps, fmtFps)
            }
        }
        let cleanName = dev.localizedName.replacingOccurrences(of: "\t", with: " ")
        lines += "\(dev.uniqueID)\t\(cleanName)\t\(maxW)\t\(maxH)\t\(maxFps)\n"
    }
    return strdup(lines)
}

@_cdecl("ts_macos_hdmi_open")
public func ts_macos_hdmi_open(
    _ deviceIdCStr: UnsafePointer<CChar>?,
    _ width: UInt32,
    _ height: UInt32,
    _ fps: UInt32
) -> UnsafeMutableRawPointer? {
    guard let deviceIdCStr else { return nil }
    requestCameraPermissionIfNeeded()
    let targetId = String(cString: deviceIdCStr)
    let devices = discoverHdmiDevices()
    guard let device = devices.first(where: { $0.uniqueID == targetId }) ?? devices.first else {
        return nil
    }
    guard let session = try? MacOsHdmiCaptureSession(device: device, width: width, height: height, fps: fps) else {
        return nil
    }
    return Unmanaged.passRetained(session).toOpaque()
}

@_cdecl("ts_macos_hdmi_set_format")
public func ts_macos_hdmi_set_format(
    _ ptr: UnsafeMutableRawPointer?,
    _ width: UInt32,
    _ height: UInt32,
    _ fps: UInt32
) -> Bool {
    guard let ptr else { return false }
    let session = Unmanaged<MacOsHdmiCaptureSession>.fromOpaque(ptr).takeUnretainedValue()
    return (try? session.configureFormat(width: width, height: height, fps: fps)) != nil
}

@_cdecl("ts_macos_hdmi_next_frame")
public func ts_macos_hdmi_next_frame(
    _ ptr: UnsafeMutableRawPointer?,
    _ outBgraPtr: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    _ outBgraLen: UnsafeMutablePointer<Int>?,
    _ outWidth: UnsafeMutablePointer<UInt32>?,
    _ outHeight: UnsafeMutablePointer<UInt32>?,
    _ outSeq: UnsafeMutablePointer<UInt64>?
) -> Bool {
    guard let ptr else { return false }
    let session = Unmanaged<MacOsHdmiCaptureSession>.fromOpaque(ptr).takeUnretainedValue()
    return session.nextFrame(
        outPtr: outBgraPtr,
        outLen: outBgraLen,
        outWidth: outWidth,
        outHeight: outHeight,
        outSeq: outSeq
    )
}

@_cdecl("ts_macos_hdmi_close")
public func ts_macos_hdmi_close(_ ptr: UnsafeMutableRawPointer?) {
    guard let ptr else { return }
    let session = Unmanaged<MacOsHdmiCaptureSession>.fromOpaque(ptr).takeRetainedValue()
    session.stop()
}
