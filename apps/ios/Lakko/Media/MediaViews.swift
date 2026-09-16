// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import ImageIO
import SwiftUI
import UIKit

/// ADR-0047 — the UIKit-backed building blocks the media bubbles, the composer
/// and the picker share: an animating image (SwiftUI `Image` shows only the
/// first frame of a GIF), a looping muted player, a player layer, a capture
/// preview, a progress ring and a waveform.

// MARK: - Animated image

/// An animated GIF / WebP (or a still) decoded by ImageIO's
/// `CGAnimateImage*WithBlock`, which drives the frame timing from the file's
/// own delays. Stops animating when the view goes away.
public struct AnimatedImageView: UIViewRepresentable {

    public enum Source: Equatable {
        case data(Data)
        case file(URL)
    }

    public let source: Source
    public var contentMode: UIView.ContentMode = .scaleAspectFill

    public init(source: Source, contentMode: UIView.ContentMode = .scaleAspectFill) {
        self.source = source
        self.contentMode = contentMode
    }

    public final class Coordinator {
        var source: Source?
        /// Set to stop the running animation block.
        var stopFlag = StopFlag()
    }

    public final class StopFlag {
        var stopped = false
    }

    public func makeCoordinator() -> Coordinator { Coordinator() }

    public func makeUIView(context: Context) -> UIImageView {
        let view = UIImageView()
        view.contentMode = contentMode
        view.clipsToBounds = true
        view.setContentHuggingPriority(.defaultLow, for: .horizontal)
        view.setContentHuggingPriority(.defaultLow, for: .vertical)
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.setContentCompressionResistancePriority(.defaultLow, for: .vertical)
        return view
    }

    public func updateUIView(_ view: UIImageView, context: Context) {
        view.contentMode = contentMode
        guard context.coordinator.source != source else { return }
        context.coordinator.stopFlag.stopped = true
        let flag = StopFlag()
        context.coordinator.stopFlag = flag
        context.coordinator.source = source
        view.image = nil
        let block: CGImageSourceAnimationBlock = { [weak view] _, image, stop in
            guard !flag.stopped, let view else {
                stop.pointee = true
                return
            }
            view.image = UIImage(cgImage: image)
        }
        let status: OSStatus
        switch source {
        case .data(let data):
            status = CGAnimateImageDataWithBlock(data as CFData, nil, block)
        case .file(let url):
            status = CGAnimateImageAtURLWithBlock(url as CFURL, nil, block)
        }
        if status != noErr {
            // Not an animation ImageIO can drive: fall back to a still.
            switch source {
            case .data(let data): view.image = UIImage(data: data)
            case .file(let url): view.image = UIImage(contentsOfFile: url.path)
            }
        }
    }

    public static func dismantleUIView(_ view: UIImageView, coordinator: Coordinator) {
        coordinator.stopFlag.stopped = true
        view.image = nil
    }
}

// MARK: - Players

/// A muted, looping player for a GIF that arrived as MP4.
public struct LoopingVideoView: UIViewRepresentable {
    public let url: URL

    public init(url: URL) {
        self.url = url
    }

    public final class PlayerView: UIView {
        override public class var layerClass: AnyClass { AVPlayerLayer.self }
        var playerLayer: AVPlayerLayer { layer as! AVPlayerLayer } // swiftlint:disable:this force_cast
        var looper: AVPlayerLooper?
        var queue: AVQueuePlayer?
        var url: URL?
    }

    public func makeUIView(context: Context) -> PlayerView {
        let view = PlayerView()
        view.playerLayer.videoGravity = .resizeAspectFill
        return view
    }

    public func updateUIView(_ view: PlayerView, context: Context) {
        guard view.url != url else { return }
        view.url = url
        let item = AVPlayerItem(asset: AVURLAsset(url: url, options: [AVURLAssetOverrideMIMETypeKey: "video/mp4"]))
        let player = AVQueuePlayer()
        player.isMuted = true
        view.looper = AVPlayerLooper(player: player, templateItem: item)
        view.queue = player
        view.playerLayer.player = player
        player.play()
    }

    public static func dismantleUIView(_ view: PlayerView, coordinator: ()) {
        view.queue?.pause()
        view.looper = nil
        view.queue = nil
        view.playerLayer.player = nil
    }
}

/// Renders an `AVPlayer` the caller owns.
public struct PlayerLayerView: UIViewRepresentable {
    public let player: AVPlayer?

    public final class LayerView: UIView {
        override public class var layerClass: AnyClass { AVPlayerLayer.self }
        var playerLayer: AVPlayerLayer { layer as! AVPlayerLayer } // swiftlint:disable:this force_cast
    }

    public func makeUIView(context: Context) -> LayerView {
        let view = LayerView()
        view.playerLayer.videoGravity = .resizeAspectFill
        return view
    }

    public func updateUIView(_ view: LayerView, context: Context) {
        if view.playerLayer.player !== player { view.playerLayer.player = player }
    }
}

/// The live front-camera preview of a video note being recorded.
public struct CapturePreviewView: UIViewRepresentable {
    public let session: AVCaptureSession?

    public final class PreviewView: UIView {
        override public class var layerClass: AnyClass { AVCaptureVideoPreviewLayer.self }
        var previewLayer: AVCaptureVideoPreviewLayer { layer as! AVCaptureVideoPreviewLayer } // swiftlint:disable:this force_cast
    }

    public func makeUIView(context: Context) -> PreviewView {
        let view = PreviewView()
        view.previewLayer.videoGravity = .resizeAspectFill
        return view
    }

    public func updateUIView(_ view: PreviewView, context: Context) {
        if view.previewLayer.session !== session { view.previewLayer.session = session }
    }
}

// MARK: - Ring + waveform

public struct ProgressRing: View {
    public let fraction: Double
    public var lineWidth: CGFloat = 4
    public var color: Color = Color.lakkoAccent

    public var body: some View {
        ZStack {
            Circle().stroke(color.opacity(0.25), lineWidth: lineWidth)
            Circle()
                .trim(from: 0, to: CGFloat(min(1, max(0, fraction))))
                .stroke(color, style: StrokeStyle(lineWidth: lineWidth, lineCap: .round))
                .rotationEffect(.degrees(-90))
        }
    }
}

/// The voice bubble / preview waveform: bars filled up to `progress`.
public struct WaveformBars: View {
    public let waveform: Data?
    public let progress: Double
    public var barCount: Int = 36
    public var tint: Color

    public var body: some View {
        let bars = Waveform.bars(waveform, count: barCount)
        GeometryReader { geo in
            HStack(alignment: .center, spacing: 2) {
                ForEach(Array(bars.enumerated()), id: \.offset) { index, value in
                    let played = Double(index) / Double(max(1, bars.count)) < progress
                    Capsule()
                        .fill(played ? tint : tint.opacity(0.35))
                        .frame(height: max(3, geo.size.height * value))
                }
            }
            .frame(maxHeight: .infinity)
        }
    }
}
