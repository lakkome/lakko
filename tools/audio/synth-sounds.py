#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors

"""
Synthesises the five call-side sound cues from pure sine tones.

All outputs are CC0 / fully Lakko-owned — no third-party samples,
no royalty-free libraries with attribution clauses, no
unattested freesound borrows. The whole point of running through
this script in CI is reproducibility: the same checked-in
parameters produce a byte-identical .wav every time on every
runner. The bash wrapper at synth-sounds.sh re-encodes the .wav
outputs into .m4a (AAC LC, 64 kbps mono, 44.1 kHz) which is the
format both iOS (AVAudioPlayer) and Android (MediaPlayer /
SoundPool) natively accept and which the web client serves
straight from <audio>.

Sounds produced:

    ringtone-incoming.wav   6.0 s   loop-friendly inbound ring
                                    (440 Hz + 480 Hz, 2 s on / 4 s off)
    ringback-outgoing.wav   4.0 s   loop-friendly outbound ringback
                                    (440 Hz alone, 1 s on / 3 s off)
    join.wav                0.2 s   ascending dual-tone (peer joined)
    leave.wav               0.2 s   descending dual-tone (peer left)
    error.wav               0.4 s   low buzz (failure cue)

No emojis, no decorative bytes, no metadata — the WAV header
is the bare PCM container the `wave` stdlib module emits.

Usage:
    python3 tools/audio/synth-sounds.py <output_dir>

The bash wrapper passes a temp dir then ffmpeg-encodes to .m4a.
"""

import math
import struct
import sys
import wave
from pathlib import Path

SAMPLE_RATE = 44_100  # Hz — matches iOS / Android default playback
AMPLITUDE = 0.40       # 40 % of full-scale — leaves headroom so
                       # downstream AAC encoding does not clip when
                       # the psychoacoustic model nudges peaks.

# ChaCha-clean stdlib RNG is not required — every sample is a
# deterministic function of the listed parameters, no randomness
# at any point.


def sine_burst(freqs_hz, duration_s, attack_s=0.005, release_s=0.010):
    """
    Produce a list of float samples in [-1.0, 1.0] for a sum-of-
    sines burst with linear attack + release envelopes.

    Attack / release default to 5 ms / 10 ms — enough to kill the
    click that would otherwise sit at the zero-crossings on
    burst boundaries.
    """
    n = int(SAMPLE_RATE * duration_s)
    samples = []
    attack_n = int(SAMPLE_RATE * attack_s)
    release_n = int(SAMPLE_RATE * release_s)
    body_n = n - attack_n - release_n

    for i in range(n):
        # Sum of sines, each at equal weight.
        v = 0.0
        for f in freqs_hz:
            v += math.sin(2.0 * math.pi * f * (i / SAMPLE_RATE))
        v /= len(freqs_hz)

        # Linear envelope. Outside [attack, n - release] we run
        # at full amplitude; the edges fade.
        if i < attack_n:
            env = i / max(1, attack_n)
        elif i >= attack_n + body_n:
            env = max(0.0, 1.0 - (i - attack_n - body_n) / max(1, release_n))
        else:
            env = 1.0

        samples.append(AMPLITUDE * env * v)
    return samples


def silence(duration_s):
    return [0.0] * int(SAMPLE_RATE * duration_s)


def chirp(start_hz, end_hz, duration_s, attack_s=0.005, release_s=0.010):
    """
    Linear-frequency chirp from start_hz to end_hz with the same
    attack / release envelope shape as `sine_burst`. The
    instantaneous frequency walks linearly between the two
    endpoints.
    """
    n = int(SAMPLE_RATE * duration_s)
    samples = []
    attack_n = int(SAMPLE_RATE * attack_s)
    release_n = int(SAMPLE_RATE * release_s)
    body_n = n - attack_n - release_n

    # Phase accumulator avoids the discontinuity a naive
    # `sin(2*pi*f(t)*t)` would introduce when f(t) varies.
    phase = 0.0
    for i in range(n):
        t = i / SAMPLE_RATE
        # Linear frequency sweep.
        f = start_hz + (end_hz - start_hz) * (t / duration_s)
        phase += 2.0 * math.pi * f / SAMPLE_RATE
        v = math.sin(phase)

        if i < attack_n:
            env = i / max(1, attack_n)
        elif i >= attack_n + body_n:
            env = max(0.0, 1.0 - (i - attack_n - body_n) / max(1, release_n))
        else:
            env = 1.0
        samples.append(AMPLITUDE * env * v)
    return samples


def write_wav(path, samples):
    """
    Encode `samples` (list of floats in [-1.0, 1.0]) to a
    16-bit mono PCM WAV at SAMPLE_RATE Hz.
    """
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)  # bytes — 16-bit PCM
        w.setframerate(SAMPLE_RATE)
        frames = bytearray()
        for s in samples:
            # Clamp + quantise; tiny epsilon to keep the
            # +32_767 endpoint reachable.
            v = max(-1.0, min(1.0, s))
            frames.extend(struct.pack("<h", int(v * 32_767)))
        w.writeframes(bytes(frames))


def make_ringtone_incoming():
    """
    Classic call-progress double-tone (440 Hz + 480 Hz, à la
    Bell System Precise Tone Plan US ringing tone), 2 s on /
    4 s off. The 6 s period loops cleanly.
    """
    s = sine_burst([440.0, 480.0], duration_s=2.0)
    s += silence(4.0)
    return s


def make_ringback_outgoing():
    """
    Quieter, single-tone ringback so the caller and the callee
    hear distinct cues. 1 s on / 3 s off, 4 s loop.
    """
    s = sine_burst([440.0], duration_s=1.0)
    s += silence(3.0)
    return s


def make_join():
    """
    200 ms ascending chirp 800 -> 1200 Hz — "someone joined".
    """
    return chirp(800.0, 1200.0, duration_s=0.20)


def make_leave():
    """
    200 ms descending chirp 1200 -> 800 Hz — "someone left".
    Mirror of `make_join` so the pair feels intentional.
    """
    return chirp(1200.0, 800.0, duration_s=0.20)


def make_error():
    """
    400 ms low double-tone (300 + 350 Hz) — distinctly lower
    than the join/leave region so it cuts through during a
    call without resembling either.
    """
    return sine_burst([300.0, 350.0], duration_s=0.40)


def main():
    if len(sys.argv) != 2:
        print("usage: synth-sounds.py <output_dir>", file=sys.stderr)
        sys.exit(2)
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)

    sounds = {
        "ringtone-incoming": make_ringtone_incoming(),
        "ringback-outgoing": make_ringback_outgoing(),
        "join": make_join(),
        "leave": make_leave(),
        "error": make_error(),
    }
    for name, samples in sounds.items():
        path = out / f"{name}.wav"
        write_wav(path, samples)
        print(f"  {name:24s} {len(samples) / SAMPLE_RATE:.2f}s -> {path}")


if __name__ == "__main__":
    main()
