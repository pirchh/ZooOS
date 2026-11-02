# Network step 1 (N1): a network device and a first IPv4 stack

Status: implemented 2026-11-02 (version 0.67.0). This is the start of the
networking pass the owner asked for after the UI and Assistant work. It
is the smallest working system: the guest gets an address from a DHCP
server and talks to its gateway.

## 1. Parts

| Part | Where | What it does |
| --- | --- | --- |
| Driver | `kernel/src/virtio_net.rs` | QEMU's `virtio-net-pci` through the legacy I/O-port interface, polled, like the block driver. 16 receive buffers of 2 KiB are always offered; one frame is sent at a time. Only the MAC feature is accepted (no offloads), so every frame has a 10-byte zero header. |
| Syscalls | `NET_ATTACH` (69), `NET_SEND` (70), `NET_RECV` (71), `NET_MAC` (72) | `NET_ATTACH` gives a handle to one live process at a time (`BUSY` otherwise), released when it ends. Frames are at most 1514 bytes; `NET_RECV` returns `AGAIN` when none is waiting. |
| Stack | `user/src/net.rs` (`zoo_rt::net`) | Ethernet framing; ARP requests, and replies to requests for our address; IPv4 with header checksums; UDP (no checksum, which IPv4 allows); ICMP echo; and a DHCP client (discover, offer, request, ack) reading the mask, router, DNS server and lease time. |
| Check | `user/src/bin/snowgoose-test.rs` | SnowGooseTest (the snow goose, a long-distance migrant) takes the device, runs DHCP, resolves the gateway and pings it three times. It exits 0 only if all of that worked. |

## 2. QEMU

- **The harness attaches** `-netdev user,…` and
  `-device virtio-net-pci,netdev=net0,disable-modern=on,romfile=`:
  - legacy, because that is what the driver speaks;
  - no boot ROM, so the firmware does not try a network boot.
- **`net-` verify cases** get `restrict=on`. QEMU's DHCP server and
  gateway answer, but the guest cannot reach the host or the internet, so
  the tests do not depend on this machine's network.
- **`cargo xtask run`** gets the user network without `restrict`, for the
  owner's own use.
- **Every other case** still has `-nic none`, as before.

## 3. Why polled, and why one holder

- **Polling** keeps the driver as small as the block driver. Interrupts
  (MSI-X, as the USB controller uses) come with a network service that
  waits for frames.
- **One holder at a time** is the same rule as the display. Later the
  holder will be a network service (a Zookeeper-supervised handler)
  that hands out sockets, and other programs will never see raw frames.
  Until then any program may take the free device: this is labelled a
  first step, not the final security model.

**What the isolated network leaves out:** with `restrict=on`, QEMU's DHCP
server sends no router and no DNS server (found on the first run: the
lease said gateway `0.0.0.0`). SnowGooseTest then uses the DHCP server's address
(`10.0.2.2`, which is also QEMU's gateway) as the next machine, and says
so.

## 4. Checks

- **`net-ping`:**
  - the shell runs `snowgoose-test` on the isolated network;
  - the DHCP offer of `10.0.2.15` comes from `10.0.2.2`;
  - the lease has mask `255.255.255.0`, gateway `10.0.2.2` and DNS
    `10.0.2.3`;
  - the gateway's MAC address is found;
  - three ping replies arrive, and SnowGooseTest exits 0.
- **`net-open`:** the same on the unrestricted network, where the lease
  names the router `10.0.2.2` and the DNS server `10.0.2.3`. QEMU answers
  DHCP, ARP and pings itself, so no internet is needed.
- **`snowgoose-no-net`** (negative): with no network device, SnowGooseTest must
  fail. The verifier must reject that run, because the exit marker
  differs.

## 5. Not done (next network steps)

- **A network service** with sockets for other programs, interrupts
  instead of polling, and DNS.
- **TCP.**
- **Reaching the internet from the desktop** (an HTTP fetch, then a
  browser).
- **Real network cards** (Intel e1000/igc, Realtek) for the owner's other
  PCs.
