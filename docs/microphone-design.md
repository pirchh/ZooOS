# Microphone: virtio-sound recording (2026-11-04)

Status: implemented for QEMU's virtio-sound, step 1 of the voice plan's
order (docs/voice-plan.md §6). A program records 16 kHz mono and
Moonshine hears it (docs/hearing-design.md). Intel HD Audio (real PCs),
voice detection and the "hey Bella" gate are not done.

## 1. The driver (`kernel/src/virtio_sound.rs`)

- **The input stream:** at boot, `PCM_INFO` for stream 1. If it is an
  input taking 16-bit samples at 16 kHz with one channel, the receive
  queue (3) is set up and the boot log says `ZOO_OS INFO sound virtio
  input 16000 Hz mono s16`. 16 kHz mono is what speech to text reads, so
  nothing is resampled.
- **Recording starts on the first read:** `PCM_SET_PARAMS` (10 periods of
  40 ms, 1280 bytes each), `PCM_PREPARE`, every receive buffer queued (a
  chain of header with the stream id, samples, status), `PCM_START`.
- **Reads never wait.** A read moves every buffer the device has filled
  into an eight-second ring, queues it again, and copies out what the ring
  holds. When nobody reads for eight seconds the oldest samples are dropped
  and counted (`dropped=` when recording stops).
- **Stopping:** `PCM_STOP`, `PCM_RELEASE`, a bounded wait for the
  released buffers, and the ring is emptied.

## 2. The system call and who may record

`AUDIO_READ` (80): `a1` buffer, `a2` bytes (at most 64 KiB). It returns
the bytes copied, 0 when nothing new is there, and `NOT_FOUND` without a
microphone. `zoo_rt::audio_read`. ABI version 20.

**One program records at a time.** The first reader owns the microphone
(`ZOO_OS INFO microphone attached pid=N`), as the network device has one
owner (docs/n1-design.md). Another program gets `BUSY` while the owner
lives. When the owner exits, recording stops (`microphone stopped`,
`microphone released`), so nothing listens after the program that asked.
The intent is that the hearing service claims it at start, as the window
server claims the display. **The `BUSY` path is not exercised by a test
yet.**

## 3. Testing without a real microphone

The development PC's microphone is never used. QEMU's SDL audio backend
with SDL's **disk** driver reads "microphone" input from a raw file:

    SDL_AUDIODRIVER=disk SDL_DISKAUDIOFILEIN=build/voice/mic-in.raw
    -audiodev sdl,id=snd0,in.mixing-engine=off,out.fixed-settings=on,...
    -device virtio-sound-pci,audiodev=snd0

- `mic-in.raw` is 1 s of silence, the host voice (Zira) saying "Hey
  Bella, open notes and maximize it." (from `hear::make_recordings`), then
  2 s of silence. `mic-silence.raw` is silence of the same length.
- **`in.mixing-engine=off` is required.** With QEMU's mixing engine and
  fixed 16 kHz mono settings, the recording came back twice as loud
  (RMS 1450 against 711) and clipped at full scale: the samples were
  changed on the way. Moonshine still heard the sentence, so a transcript
  alone would not have caught it. With the engine off, the device opens
  SDL in the guest's own format and the samples arrive unchanged.
- **ReindeerTest** (`user/src/bin/reindeer-test.rs`) records 6 s, then:
  1. requires one second from the first sound to equal the same second of
     `test/mic.raw` (the source, in the hearing image), or it exits 4;
  2. transcribes with Moonshine and requires `test/mic.txt`'s sentence.

| Case | Result |
| --- | --- |
| `mic-hear` (TCG) | `OK`: 6000 ms recorded in 6039 ms; "1 s from the first sound (at 1117 ms) equals the source"; heard "Hey Bella, open notes and maximize it." in 19.3 s |
| `mic-hear` (WHPX, 8 processors) | `OK`: heard in 217 ms (6 s of audio, half of it silence) |
| `mic-silence` (negative case) | rejected as intended: "no sound recorded", MISHEARD "You", `code=1` |
| `mic-hear` with the mixing engine on (negative control) | `FAIL`: "15178 of 16000 samples differ from the source", `code=4` |

Moonshine writes "You" for silence. Voice detection (Silero VAD) is
needed before Bella listens all the time.

## Not done

- Intel HD Audio for real PCs; this driver is QEMU's virtio-sound only.
- Voice detection, the gate and BatHandler: done since 0.112.0
  (docs/listening-design.md).
- Interrupts instead of polling; choosing among several input streams.
