# Hardware step 4 (H4): USB hubs

Status: implemented 2026-10-31 (version 0.34.0). Addresses a risk in
docs/usb-boot.md: front-panel ports, monitors, docks and many laptops'
internal keyboards connect through a hub, and the xHCI driver only saw
devices on its own root ports.

## 1. Design

- Enumeration is split: `enumerate` resets a root port; `attach` sets up
  the device at a `Location` (root port, hub port, route string, speed,
  and the transaction translator for a full- or low-speed device behind a
  high-speed hub). Slot contexts carry the route string and, when needed,
  the translator's hub slot and port, both for Address Device and for the
  HID endpoint's Configure Endpoint.
- A device of class 9 on a root port is a hub (one tier):
  1. GET_DESCRIPTOR(hub): number of ports, power-on time.
  2. SET_CONFIGURATION, then Configure Endpoint with the slot context's
     Hub flag and number of ports.
  3. SET_FEATURE(PORT_POWER) on every port; wait the power-on time
     (spins: ticks do not run during enumeration).
  4. For each connected port: clear C_PORT_CONNECTION,
     SET_FEATURE(PORT_RESET), poll GET_STATUS for C_PORT_RESET (bounded),
     clear it, check enabled, read the speed, and `attach` the device.
- Logs use the location: `usb port=5.1 hid=keyboard`.

## 2. Verification

- `usb-hub` case (QEMU `usb-hub` on root port 1, keyboard on hub port 1,
  mouse on hub port 2): `port=5 … class=9`, `port=5.1 hid=keyboard`,
  `5.1 input ready`, `port=5.2 hid=mouse`, `5.2 input ready`, `devices=3`;
  `echo hub` typed through the keyboard behind the hub reaches the shell.
- `usb`, `usb-desktop` (devices on root ports) pass unchanged.
- The `usb` test option is now a choice: none, direct, or behind a hub.

Negative control: the hub port left out of the route string →
`port=5.1 error=command failed`, `port=5.2 error=command failed`,
`devices=1`; `FAIL usb-hub`.

## 3. Not done

- One tier of hubs (no hub behind a hub); USB 3 hubs (which need the
  SuperSpeed hub descriptor) untested.
- The transaction-translator fields are set but QEMU's hub is full speed,
  so the high-speed-hub path is untested until real hardware.
- No hot-plug: devices must be connected at boot.
