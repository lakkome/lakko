// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import Foundation
import SwiftUI
import UIKit

/// `QrScannerView` — SwiftUI wrapper around `AVCaptureSession`
/// for QR-code scanning (Phase 2a3b3-ui).
///
/// `AVCaptureMetadataOutput` natively recognises QR codes —
/// no third-party scanner library needed (mirror of the
/// `qrcode` + `jsqr` pair on the web side, but iOS has the
/// equivalent surface built in).
///
/// The view publishes scanned text via the `onDetected`
/// closure. The closure runs on the main actor; the caller is
/// expected to stop the session by changing its state (e.g.
/// switching the parent's state machine to `.processing`) and
/// removing this view from the hierarchy — the `Coordinator`'s
/// `deinit` then tears down the `AVCaptureSession`.
///
/// The decoded text payload is the base64url-no-pad form of
/// the pairing-QR wire bytes (matches the encoding side in
/// ``QrImageView``).
public struct QrScannerView: UIViewControllerRepresentable {

    public var onDetected: (String) -> Void
    public var onError: (String) -> Void

    public init(
        onDetected: @escaping (String) -> Void,
        onError: @escaping (String) -> Void
    ) {
        self.onDetected = onDetected
        self.onError = onError
    }

    public func makeCoordinator() -> Coordinator {
        Coordinator(onDetected: onDetected, onError: onError)
    }

    public func makeUIViewController(context: Context) -> ScannerViewController {
        let vc = ScannerViewController()
        vc.delegate = context.coordinator
        return vc
    }

    public func updateUIViewController(
        _ uiViewController: ScannerViewController,
        context: Context
    ) {
        // No external state to push down — the controller
        // owns its AVCaptureSession lifecycle. SwiftUI calls
        // this on each parent re-render; nothing to do.
    }

    public final class Coordinator: NSObject, ScannerViewControllerDelegate {
        let onDetected: (String) -> Void
        let onError: (String) -> Void

        init(
            onDetected: @escaping (String) -> Void,
            onError: @escaping (String) -> Void
        ) {
            self.onDetected = onDetected
            self.onError = onError
        }

        public func scanner(_ vc: ScannerViewController, didDetect text: String) {
            onDetected(text)
        }

        public func scanner(_ vc: ScannerViewController, didFail message: String) {
            onError(message)
        }
    }
}

// MARK: - UIKit controller

public protocol ScannerViewControllerDelegate: AnyObject {
    func scanner(_ vc: ScannerViewController, didDetect text: String)
    func scanner(_ vc: ScannerViewController, didFail message: String)
}

public final class ScannerViewController: UIViewController,
    AVCaptureMetadataOutputObjectsDelegate
{
    public weak var delegate: ScannerViewControllerDelegate?

    private let session = AVCaptureSession()
    private var previewLayer: AVCaptureVideoPreviewLayer?
    /// Flipped to `true` on first detection so the per-frame
    /// callback doesn't fire the delegate multiple times for
    /// the same QR before SwiftUI has had a chance to tear
    /// down the view.
    private var hasDetected: Bool = false

    public override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        configureSession()
    }

    public override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        previewLayer?.frame = view.layer.bounds
    }

    public override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        if !session.isRunning {
            // AVCaptureSession.startRunning is a blocking
            // call (can take >100ms on cold start). Run on a
            // background queue so the main thread keeps
            // responsive.
            DispatchQueue.global(qos: .userInitiated).async { [weak self] in
                self?.session.startRunning()
            }
        }
    }

    public override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        if session.isRunning {
            session.stopRunning()
        }
    }

    private func configureSession() {
        session.beginConfiguration()
        session.sessionPreset = .high

        guard
            let device = AVCaptureDevice.default(
                .builtInWideAngleCamera,
                for: .video,
                position: .back
            )
        else {
            session.commitConfiguration()
            delegate?.scanner(self, didFail: Localization.text("devices.scan.error.no_camera"))
            return
        }

        let input: AVCaptureDeviceInput
        do {
            input = try AVCaptureDeviceInput(device: device)
        } catch {
            session.commitConfiguration()
            NSLog("[pairing] WARN camera input failed: \(error)")
            delegate?.scanner(self, didFail: Localization.text("devices.scan.error.camera_denied"))
            return
        }
        guard session.canAddInput(input) else {
            session.commitConfiguration()
            delegate?.scanner(self, didFail: Localization.text("devices.scan.error.camera_failed"))
            return
        }
        session.addInput(input)

        let output = AVCaptureMetadataOutput()
        guard session.canAddOutput(output) else {
            session.commitConfiguration()
            delegate?.scanner(self, didFail: Localization.text("devices.scan.error.camera_failed"))
            return
        }
        session.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: .main)
        output.metadataObjectTypes = [.qr]

        let preview = AVCaptureVideoPreviewLayer(session: session)
        preview.videoGravity = .resizeAspectFill
        preview.frame = view.layer.bounds
        view.layer.addSublayer(preview)
        self.previewLayer = preview

        session.commitConfiguration()
    }

    public func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        if hasDetected { return }
        for object in metadataObjects {
            guard
                let readable = object as? AVMetadataMachineReadableCodeObject,
                readable.type == .qr,
                let text = readable.stringValue
            else { continue }
            hasDetected = true
            delegate?.scanner(self, didDetect: text)
            break
        }
    }

    deinit {
        if session.isRunning {
            session.stopRunning()
        }
    }
}
