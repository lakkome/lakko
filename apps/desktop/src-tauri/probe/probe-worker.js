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
  // The rate probe: a 50 ms timer in the worker, posted to the page.
  if (event.data === "tick-start") setInterval(() => self.postMessage("tick"), 50);
  if (event.data && event.data.tickEvery) setInterval(() => self.postMessage("tick"), event.data.tickEvery);
};
