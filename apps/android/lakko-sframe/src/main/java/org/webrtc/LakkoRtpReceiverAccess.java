// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package org.webrtc;

/**
 * Reads the engine pointer of an {@link RtpReceiver}.
 *
 * <p>{@code RtpReceiver.getNativeRtpReceiver()} is package-private in the
 * SDK ({@code io.github.webrtc-sdk:android}): it returns the raw
 * {@code webrtc::RtpReceiverInterface*} the Java object holds one reference
 * on, and throws {@link IllegalStateException} once the object is disposed.
 * This class lives in the SDK's package for the one purpose of calling it,
 * the way the SDK's own classes do. Nothing is reflected and no private
 * member is touched. The SDK's jar is not sealed, and the app loads it and
 * this class through one class loader, so they share a runtime package.
 *
 * <p>Used by {@code me.lakko.sframe.ReceiverPlayoutDelay} only (ADR-0070,
 * note of 2026-10-12).
 */
public final class LakkoRtpReceiverAccess {
  private LakkoRtpReceiverAccess() {}

  /**
   * The engine pointer of {@code receiver}; never 0.
   *
   * @throws IllegalStateException when {@code receiver} has been disposed.
   */
  public static long nativePointer(RtpReceiver receiver) {
    return receiver.getNativeRtpReceiver();
  }
}
