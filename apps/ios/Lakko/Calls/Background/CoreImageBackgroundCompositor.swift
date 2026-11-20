// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreImage
import CoreVideo
import Foundation
import ImageIO
import LakkoCore
import Metal
import WebRTC

/// The camera background's pixel work on the GPU through Core Image
/// (ADR-0089 section 2).
///
/// Everything is done with the frame turned upright (the camera's buffer is
/// the sensor's landscape picture plus a rotation): the model sees the person
/// upright, a picture fills the frame the way it is shown, and the frame
/// handed on is upright with no rotation. The self view mirrors it as before.
///
/// - Model input: the upright frame with its shape kept, drawn into the
///   Rust `backgroundModelInputRect` inside a black 256x144 picture (bars
///   where the shapes differ, as on the other clients), BGRA, rows top to
///   bottom, not mirrored. The mask is read back through the same rectangle.
/// - Blur: the frame scaled down by the Rust `divisor`, a Gaussian of the
///   Rust `sigma` (Core Image's blur radius is the standard deviation),
///   scaled back up, then `out = frame * alpha + blurred * (1 - alpha)`.
/// - Picture: a built-in drawn by the Rust renderer at the frame's size, or
///   an own photo cropped by the Rust cover rect, then the same blend.
///
/// One `CIContext` for the process; one output pool per frame size.
final class CoreImageBackgroundCompositor: BackgroundCompositing {

    private let context: CIContext
    private let colorSpace = CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB()

    private let lock = NSLock()
    private var pool: CVPixelBufferPool?
    private var poolSize: (width: Int, height: Int) = (0, 0)
    /// The picture behind the person, for the last effect and frame size.
    private var pictureKey: String?
    private var picture: CIImage?
    /// Where a frame of the last size sits in the model's picture.
    private var modelRectSize: (width: Int, height: Int) = (0, 0)
    private var modelRectCached: CGRect?
    /// The Rust blur parameters, for the last strength and frame height.
    private var blurKey: String?
    private var blurParameters: BackgroundBlur?

    /// `nil` on a device with no Metal: the background is then not offered
    /// (UX section 3, "this device cannot run a background at all").
    init?() {
        guard let device = MTLCreateSystemDefaultDevice() else { return nil }
        context = CIContext(
            mtlDevice: device,
            options: [.cacheIntermediates: false, .name: "lakko-call-background"])
    }

    // MARK: - BackgroundCompositing

    func modelInput(_ frame: RTCVideoFrame) -> [UInt8]? {
        guard let upright = Self.upright(frame) else { return nil }
        let size = upright.extent.size
        guard size.width > 0, size.height > 0, let rect = modelRect(for: size) else { return nil }
        let width = CGFloat(BackgroundModelSize.width)
        let height = CGFloat(BackgroundModelSize.height)
        let model = CGRect(x: 0, y: 0, width: width, height: height)
        let scaled = upright
            .transformed(by: Self.frameToModel(frame: size, rect: rect))
            .composited(over: CIImage(color: .black).cropped(to: model))
            .cropped(to: model)
        var bytes = [UInt8](repeating: 0, count: BackgroundModelSize.inputBytes)
        bytes.withUnsafeMutableBytes { raw in
            guard let base = raw.baseAddress else { return }
            context.render(
                scaled,
                toBitmap: base,
                rowBytes: BackgroundModelSize.width * 4,
                bounds: CGRect(x: 0, y: 0, width: width, height: height),
                format: .BGRA8,
                colorSpace: colorSpace)
        }
        return bytes
    }

    func composite(
        _ frame: RTCVideoFrame, mask: [UInt8], effect: BackgroundEffect
    ) -> RTCVideoFrame? {
        guard mask.count == BackgroundModelSize.maskBytes,
              let base = Self.upright(frame) else { return nil }
        let extent = base.extent
        let width = Int(extent.width.rounded())
        let height = Int(extent.height.rounded())
        guard width > 0, height > 0, let rect = modelRect(for: extent.size) else { return nil }

        let maskImage = CIImage(
            bitmapData: Data(mask),
            bytesPerRow: BackgroundModelSize.width,
            size: CGSize(width: BackgroundModelSize.width, height: BackgroundModelSize.height),
            format: .L8,
            colorSpace: nil)
            // Only the frame's own rectangle: the bars never bleed into the
            // edge of the upsampled mask.
            .cropped(to: rect)
            .clampedToExtent()
            .samplingLinear()
            .transformed(by: Self.frameToModel(frame: extent.size, rect: rect).inverted())
            .cropped(to: extent)

        guard let behind = background(for: effect, base: base, width: width, height: height),
              let blend = CIFilter(name: "CIBlendWithMask")
        else { return nil }
        blend.setValue(base, forKey: kCIInputImageKey)
        blend.setValue(behind, forKey: kCIInputBackgroundImageKey)
        blend.setValue(maskImage, forKey: kCIInputMaskImageKey)
        guard let output = blend.outputImage?.cropped(to: extent),
              let buffer = outputBuffer(width: width, height: height)
        else { return nil }
        context.render(output, to: buffer, bounds: extent, colorSpace: colorSpace)
        return RTCVideoFrame(
            buffer: RTCCVPixelBuffer(pixelBuffer: buffer),
            rotation: ._0,
            timeStampNs: frame.timeStampNs)
    }

    // MARK: - Where the frame sits in the model's picture

    /// The Rust model-input rectangle for a frame of `size`, in Core Image
    /// coordinates (origin bottom left) of the 256x144 picture. Cached for
    /// the last frame size.
    private func modelRect(for size: CGSize) -> CGRect? {
        let width = Int(size.width.rounded())
        let height = Int(size.height.rounded())
        lock.lock()
        if modelRectSize.width == width, modelRectSize.height == height, let cached = modelRectCached {
            lock.unlock()
            return cached
        }
        lock.unlock()
        let made = Self.modelRect(frameWidth: width, frameHeight: height)
        lock.lock()
        modelRectSize = (width, height)
        modelRectCached = made
        lock.unlock()
        return made
    }

    /// ``modelRect(for:)`` uncached: `backgroundModelInputRect` (top-left
    /// origin) flipped to Core Image's bottom-left origin. `nil` for an empty
    /// rectangle.
    static func modelRect(frameWidth: Int, frameHeight: Int) -> CGRect? {
        let r = backgroundModelInputRect(
            frameWidth: UInt32(clamping: frameWidth), frameHeight: UInt32(clamping: frameHeight))
        guard r.width > 0, r.height > 0 else { return nil }
        return CGRect(
            x: CGFloat(r.x),
            y: CGFloat(BackgroundModelSize.height) - CGFloat(r.y) - CGFloat(r.height),
            width: CGFloat(r.width),
            height: CGFloat(r.height))
    }

    /// The map from an upright frame of `frame` size onto `rect` of the
    /// model's picture: one scale for both axes when the rectangle keeps the
    /// shape, so nothing is stretched. Its inverse brings the mask back.
    static func frameToModel(frame: CGSize, rect: CGRect) -> CGAffineTransform {
        CGAffineTransform(translationX: rect.minX, y: rect.minY)
            .scaledBy(x: rect.width / frame.width, y: rect.height / frame.height)
    }

    // MARK: - The picture behind

    private func background(
        for effect: BackgroundEffect, base: CIImage, width: Int, height: Int
    ) -> CIImage? {
        switch effect {
        case .none:
            return nil
        case .blur(let strong):
            return blurred(base, strong: strong, height: height)
        case .builtin(let id):
            return cachedPicture(key: "builtin:\(id):\(width)x\(height)") {
                Self.builtinImage(id: id, width: width, height: height)
            }
        case .photo(let id, let image):
            return cachedPicture(key: "photo:\(id):\(width)x\(height)") {
                Self.cover(image, width: width, height: height)
            }
        }
    }

    private func cachedPicture(key: String, make: () -> CIImage?) -> CIImage? {
        lock.lock()
        if pictureKey == key, let picture {
            lock.unlock()
            return picture
        }
        lock.unlock()
        guard let made = make() else { return nil }
        lock.lock()
        pictureKey = key
        picture = made
        lock.unlock()
        return made
    }

    private func blurred(_ base: CIImage, strong: Bool, height: Int) -> CIImage? {
        let key = "\(strong):\(height)"
        lock.lock()
        var parameters = blurKey == key ? blurParameters : nil
        lock.unlock()
        if parameters == nil {
            let made = backgroundBlur(strong: strong, frameHeight: UInt32(clamping: height))
            lock.lock()
            blurKey = key
            blurParameters = made
            lock.unlock()
            parameters = made
        }
        guard let parameters, parameters.divisor > 0 else { return nil }
        let divisor = CGFloat(parameters.divisor)
        let extent = base.extent
        let small = base
            .transformed(by: CGAffineTransform(scaleX: 1 / divisor, y: 1 / divisor))
        let smallExtent = small.extent
        guard let blur = CIFilter(name: "CIGaussianBlur") else { return nil }
        blur.setValue(small.clampedToExtent(), forKey: kCIInputImageKey)
        blur.setValue(NSNumber(value: parameters.sigma), forKey: kCIInputRadiusKey)
        guard let blurredSmall = blur.outputImage?.cropped(to: smallExtent) else { return nil }
        return blurredSmall
            .samplingLinear()
            .transformed(by: CGAffineTransform(scaleX: divisor, y: divisor))
            .clampedToExtent()
            .cropped(to: extent)
    }

    /// A built-in background, drawn by the Rust renderer at the frame's size.
    static func builtinImage(id: String, width: Int, height: Int) -> CIImage? {
        guard let rgba = backgroundBuiltinRender(
            id: id, width: UInt32(clamping: width), height: UInt32(clamping: height)),
              rgba.count == width * height * 4
        else { return nil }
        return CIImage(
            bitmapData: rgba,
            bytesPerRow: width * 4,
            size: CGSize(width: width, height: height),
            format: .RGBA8,
            colorSpace: CGColorSpace(name: CGColorSpace.sRGB))
    }

    /// An own photo filling `width` x `height`, cropped from the centre by
    /// the Rust cover rect (top-left origin, in the photo's pixels).
    static func cover(_ image: CGImage, width: Int, height: Int) -> CIImage? {
        let sourceWidth = image.width
        let sourceHeight = image.height
        guard sourceWidth > 0, sourceHeight > 0 else { return nil }
        let rect = backgroundCoverRect(
            srcWidth: UInt32(clamping: sourceWidth), srcHeight: UInt32(clamping: sourceHeight),
            dstWidth: UInt32(clamping: width), dstHeight: UInt32(clamping: height))
        guard rect.width > 0, rect.height > 0 else { return nil }
        // Core Image's origin is the bottom left.
        let crop = CGRect(
            x: CGFloat(rect.x),
            y: CGFloat(sourceHeight) - CGFloat(rect.y) - CGFloat(rect.height),
            width: CGFloat(rect.width),
            height: CGFloat(rect.height))
        return CIImage(cgImage: image)
            .cropped(to: crop)
            .transformed(by: CGAffineTransform(translationX: -crop.minX, y: -crop.minY))
            .transformed(by: CGAffineTransform(
                scaleX: CGFloat(width) / crop.width, y: CGFloat(height) / crop.height))
            .clampedToExtent()
            .cropped(to: CGRect(x: 0, y: 0, width: width, height: height))
    }

    // MARK: - Frames

    /// The frame's picture turned upright, its extent at the origin. `nil`
    /// for a buffer that is not a pixel buffer (the camera always gives one).
    static func upright(_ frame: RTCVideoFrame) -> CIImage? {
        guard let cv = frame.buffer as? RTCCVPixelBuffer else { return nil }
        var image = CIImage(cvPixelBuffer: cv.pixelBuffer)
        let full = CGSize(
            width: CVPixelBufferGetWidth(cv.pixelBuffer),
            height: CVPixelBufferGetHeight(cv.pixelBuffer))
        if cv.cropWidth > 0, cv.cropHeight > 0,
           CGFloat(cv.cropWidth) != full.width || CGFloat(cv.cropHeight) != full.height {
            // Pixel-buffer rows run top to bottom; Core Image's from the
            // bottom.
            let crop = CGRect(
                x: CGFloat(cv.cropX),
                y: full.height - CGFloat(cv.cropY) - CGFloat(cv.cropHeight),
                width: CGFloat(cv.cropWidth),
                height: CGFloat(cv.cropHeight))
            image = image.cropped(to: crop)
        }
        image = image.oriented(orientation(frame.rotation))
        return image.transformed(by: CGAffineTransform(
            translationX: -image.extent.minX, y: -image.extent.minY))
    }

    /// The EXIF orientation that turns a frame of `rotation` upright.
    static func orientation(_ rotation: RTCVideoRotation) -> CGImagePropertyOrientation {
        switch rotation {
        case ._90: return .right
        case ._180: return .down
        case ._270: return .left
        default: return .up
        }
    }

    private func outputBuffer(width: Int, height: Int) -> CVPixelBuffer? {
        lock.lock()
        defer { lock.unlock() }
        if pool == nil || poolSize.width != width || poolSize.height != height {
            let attributes: [String: Any] = [
                kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
                kCVPixelBufferWidthKey as String: width,
                kCVPixelBufferHeightKey as String: height,
                kCVPixelBufferIOSurfacePropertiesKey as String: [String: Any](),
                kCVPixelBufferMetalCompatibilityKey as String: true,
            ]
            var made: CVPixelBufferPool?
            CVPixelBufferPoolCreate(nil, nil, attributes as CFDictionary, &made)
            pool = made
            poolSize = (width, height)
        }
        guard let pool else { return nil }
        var buffer: CVPixelBuffer?
        CVPixelBufferPoolCreatePixelBuffer(nil, pool, &buffer)
        return buffer
    }
}
