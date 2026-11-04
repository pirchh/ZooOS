//! OrcaTest: the sound test (docs/audio-design.md). Plays one second of a
//! 440 Hz tone, 16-bit stereo at 48 kHz, through `audio_write`; the
//! harness records QEMU's output to a WAV file and checks its pitch.
//! Exits 0 when all of it was taken, 2 without a sound device, 3 if the
//! device stopped taking samples.

#![no_std]
#![no_main]

use zoo_rt::Error;

zoo_rt::entry!(main);

const RATE: u32 = 48_000;
const TONE_HZ: u32 = 440;
const SECONDS: u32 = 1;
/// Frames written per call: 100 ms.
const CHUNK: usize = 4800;
/// Ticks to keep trying while the device takes nothing (10 ms each).
const PATIENCE: u32 = 300;

/// sin(2 pi t) for t in [0, 1): Bhaskara I's approximation, about 0.2%
/// off at worst, plenty for a test tone.
fn sine(t: f32) -> f32 {
    let (x, sign) = if t < 0.5 { (t, 1.0) } else { (t - 0.5, -1.0) };
    // On [0, pi]: 16 y (pi - y) / (5 pi^2 - 4 y (pi - y)), with y = 2 pi x.
    let pi = core::f32::consts::PI;
    let y = 2.0 * pi * x;
    let p = y * (pi - y);
    sign * 16.0 * p / (5.0 * pi * pi - 4.0 * p)
}

fn main(_start: u64) -> u8 {
    let total = (RATE * SECONDS) as usize;
    let mut buf = [0u8; CHUNK * 4];
    let mut frame = 0usize;
    let mut idle = 0;
    while frame < total {
        let n = (total - frame).min(CHUNK);
        for k in 0..n {
            let t =
                ((frame + k) as u64 * u64::from(TONE_HZ) % u64::from(RATE)) as f32 / RATE as f32;
            let s = (sine(t) * 12_000.0) as i16;
            let b = s.to_le_bytes();
            buf[4 * k..4 * k + 4].copy_from_slice(&[b[0], b[1], b[0], b[1]]);
        }
        // Hand over this chunk, as much as the device takes each time.
        let bytes = &buf[..4 * n];
        let mut done = 0;
        while done < bytes.len() {
            match zoo_rt::audio_write(&bytes[done..]) {
                Ok(0) => {
                    idle += 1;
                    if idle > PATIENCE {
                        let _ = zoo_rt::log("OrcaTest: the sound device stopped taking samples");
                        return 3;
                    }
                    zoo_rt::sleep(1);
                }
                Ok(k) => {
                    idle = 0;
                    done += k;
                }
                Err(Error::NotFound) => {
                    let _ = zoo_rt::log("OrcaTest: no sound device");
                    return 2;
                }
                Err(e) => {
                    zoo_rt::log!("OrcaTest: audio_write failed: {}", e.name());
                    return 3;
                }
            }
        }
        frame += n;
    }
    zoo_rt::log!("OrcaTest: played {} ms of {} Hz", SECONDS * 1000, TONE_HZ);
    0
}
