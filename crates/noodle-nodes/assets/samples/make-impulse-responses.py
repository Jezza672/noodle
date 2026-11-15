#!/usr/bin/env python3
"""Makes the three impulse responses the Convolution Reverb template ships with.

They are synthetic (decaying, band-split noise with a few early reflections),
made by this script, so they have no source to credit: they are part of Noodle
and under its licence. Nothing here is random: a fixed generator, so running it
again gives the same files (and so the same content hashes in index.txt).

    python3 make-impulse-responses.py
"""
import math
import struct
import wave

RATE = 44_100


class Lcg:
    def __init__(self, seed):
        self.state = seed

    def uniform(self):
        self.state = (self.state * 1664525 + 1013904223) & 0xFFFFFFFF
        return (self.state >> 8) / (1 << 23) - 1.0

    def gauss(self):
        return (self.uniform() + self.uniform() + self.uniform()) / 1.7320508


def one_pole(x, hz):
    a = math.exp(-2.0 * math.pi * hz / RATE)
    y, out = 0.0, []
    for v in x:
        y = (1.0 - a) * v + a * y
        out.append(y)
    return out


def channel(seed, seconds, rt60, rise, predelay, early, bright):
    rng = Lcg(seed)
    n = int(seconds * RATE)
    noise = [rng.gauss() for _ in range(n)]
    low = one_pole(noise, 250.0)
    low2 = one_pole(low, 250.0)
    mid_lp = one_pole(one_pole(noise, 3000.0), 3000.0)
    bands = [
        (low2, rt60 * 1.25),
        ([m - l for m, l in zip(mid_lp, low2)], rt60),
        ([x - m for x, m in zip(noise, mid_lp)], rt60 * bright),
    ]
    out = [0.0] * n
    start = int(predelay * RATE)
    for samples, rt in bands:
        for i in range(start, n):
            t = (i - start) / RATE
            out[i] += samples[i] * math.exp(-6.908 * t / rt) * (1.0 - math.exp(-t / rise))
    # A few discrete early reflections, different in each channel.
    for k in range(early):
        at = start + int((0.004 + 0.036 * (k + rng.uniform() * 0.5 + 0.5) / early) * RATE)
        if at < n:
            out[at] += (1.0 if rng.uniform() > 0 else -1.0) * 0.6 * math.exp(-k * 0.35)
    fade = int(0.01 * RATE)
    for i in range(fade):
        out[n - 1 - i] *= i / fade
    return out


def write(name, left, right):
    peak = max(max(abs(x) for x in left), max(abs(x) for x in right))
    scale = 0.9 / peak
    with wave.open(f"{name}.wav", "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(RATE)
        frames = bytearray()
        for l, r in zip(left, right):
            frames += struct.pack("<hh", int(l * scale * 32767), int(r * scale * 32767))
        w.writeframes(bytes(frames))


# name, seconds, RT60, rise time, pre-delay, early reflections, treble decay
for name, seconds, rt60, rise, predelay, early, bright, seeds in [
    ("ir-room", 0.5, 0.35, 0.003, 0.0, 9, 0.6, (11, 12)),
    ("ir-hall", 2.4, 1.9, 0.03, 0.015, 6, 0.45, (21, 22)),
    ("ir-plate", 1.5, 1.2, 0.002, 0.0, 0, 0.9, (31, 32)),
]:
    write(
        name,
        channel(seeds[0], seconds, rt60, rise, predelay, early, bright),
        channel(seeds[1], seconds, rt60, rise, predelay, early, bright),
    )
