# Hardware step 3 (H3): the local APIC timer

Status: implemented 2026-10-31 (version 0.33.0). Addresses the first risk
in docs/usb-boot.md: ZooOS's only tick was the legacy PIT (IRQ0), whose
clock some recent Intel platforms gate, which would stop the system at
the timer self-test.

## 1. Design

- `kernel/src/apic.rs`. At timer start-up (interrupts still off):
  1. Find the ACPI PM timer: the RSDP address from the bootloader → XSDT
     (or RSDT on ACPI 1.0) → FADT → `PM_TMR_BLK` (or `X_PM_TMR_BLK` when
     it is in I/O space); 24- or 32-bit as the FADT's `TMR_VAL_EXT` says.
  2. Check CPUID for a local APIC, enable it in `IA32_APIC_BASE`, map its
     page uncached (`map_mmio`), set the spurious vector (0xFF, a handler
     that does nothing) and the task priority.
  3. Calibrate: count the APIC timer down (divide by 16) from 2³²−1 for
     one tick's worth of PM-timer counts (35,795 at 3.579545 MHz, 10 ms),
     bounded; scale by the PM counts actually elapsed.
  4. Run the APIC timer periodically on the same vector the PIT used
     (32), so the trap path and scheduler are unchanged; IRQ0 stays
     masked; the tick handler sends the APIC's EOI instead of the PIC's.
- Logs `ZOO_OS INFO timer source=lapic per-tick=N pm-timer=0x…`. Any
  missing piece (no ACPI, no FADT or PM timer, no APIC, a PM timer that
  does not count, a calibration out of range) logs `source=pit
  reason=…` and the PIT runs as before.

## 2. Verification

- Every case now runs on the APIC timer (QEMU q35: PM timer at `0x608`).
- New case `timer-rate`: `tern-test wait` logs, sleeps 500 ticks, logs
  again; the harness (new action `WaitGap`) requires the second line 4–6
  s of host time after the first. Measured: 4,965 ms (TCG), 4,958 ms
  (WHPX).
- `fast-boot` under WHPX: passes with `source=lapic`.
- The PIT fallback, forced by a temporary edit: `timer-rate` 4,958 ms and
  `desktop` pass with `source=pit reason=forced-for-test`.

Negative control: calibration doubled → `FAIL timer-rate`, `waited came
10012 ms after the previous line (expected 4000-6000 ms)`.

## 3. Not done

- Interrupts other than the timer still go through the legacy PICs (no
  I/O APIC); USB is polled on the tick.
- If a PC has neither a working PIT nor a PM timer the APIC timer cannot
  be calibrated (HPET or CPUID frequency leaves would be next).
- One CPU only.
