# Sound: virtio-sound playback (2026-11-04)

Status: implemented for QEMU's virtio-sound, output only. The first piece
of voice (docs/voice-plan.md): Bella will need to speak. Needs no
download. Real PCs have Intel HD Audio, a later driver; recording
(microphone) is not done.

## 1. The driver (`kernel/src/virtio_sound.rs`)

- **The device:** virtio-sound (1AF4:1059) on the modern virtio PCI
  transport, set up as the GPU driver is (docs/gpu1-design.md): reset,
  `VIRTIO_F_VERSION_1` only, polled queues without interrupts. The
  control queue (0) and the transmit queue (2) are used; the event and
  receive queues stay disabled.
- **The stream:** at boot, `PCM_INFO` must say stream 0 is an output that
  takes 16-bit samples at 48 kHz in stereo, or the driver stays off
  (`ZOO_OS INFO sound error=...`). On the first write: `PCM_SET_PARAMS`
  (8 periods of 25 ms, 4800 bytes each), `PCM_PREPARE`, `PCM_START`.
- **Never waiting for the speaker:** the device takes samples at playing
  speed, so a write that waited would hold the scheduler lock for as long
  as the sound lasts (docs/keys-design.md showed what that does to the
  keyboard). `write` copies what fits into free period buffers (each a
  chain: transfer header, samples, status), hands them over, and returns
  the bytes it took; buffers the device has played are taken back on the
  next call. At most 200 ms is queued ahead.

## 2. The system call

`AUDIO_WRITE` (79): `a1` samples (16-bit little-endian stereo at 48 kHz),
`a2` bytes (at most 64 KiB). Returns the bytes taken now, 0 when every
buffer is in use (the caller sleeps a tick and tries again),
`NOT_FOUND` without a sound device, `BUSY` if the stream would not start.
`zoo_rt::audio_write`. ABI version 19.

## 3. The test

- **OrcaTest** (`user/src/bin/orca-test.rs`) plays one second of 440 Hz
  (a sine from Bhaskara's approximation, at a sixth of full scale).
- **QEMU** gets `-audiodev wav,id=snd0,path=build/runs/<case>/audio.wav
  -device virtio-sound-pci,audiodev=snd0` for `audio-` cases: its WAV
  backend records what the guest plays (resampled to 44.1 kHz).
- **The harness** (`check_tone`) reads the WAV file, takes the loud part
  of the first channel and counts rising zero crossings: it must last at
  least 0.8 s and be within 2% of 440 Hz.

| Case | Result |
| --- | --- |
| `audio-tone` | `OK`: "audio: 439.8 Hz for 1.00 s (44100 Hz, 2 channels recorded)" |
| `audio-silence` (negative: the same machine, nothing played) | rejected as intended: no tone (QEMU writes no samples) |
| `audio-tone` with OrcaTest playing 880 Hz (negative control) | `FAIL`: "tone at 879.6 Hz, wanted 440 Hz" |

## Not done

- Recording (the receive queue): QEMU's WAV backend cannot feed a
  microphone, so a recording test needs another way in
  (docs/voice-plan.md §4).
- Intel HD Audio for real PCs; volume; mixing several programs' sound
  (one at a time for now); interrupts instead of polling.
