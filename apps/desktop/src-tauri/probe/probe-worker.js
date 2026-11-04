// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// S1 probe worker: accepts an RTCRtpScriptTransform and passes frames
// through unchanged. Only its construction is measured.
self.onrtctransform = (event) => {
  const t = event.transformer;
  t.readable.pipeTo(t.writable).catch(() => {});
};
self.onmessage = (event) => {
  if (event.data === "ping") self.postMessage("pong");
};
