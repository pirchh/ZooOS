# Network step 3 (N3): the Network app and DNS

Status: implemented 2026-11-02 (version 0.68.0). The network gets a face
on the desktop: a Network window that shows the connection and fetches a
web page by name or address.

## 1. The Network app (`user/src/bin/network.rs`)

- **Where it lives:** a fifth application (`PROGRAMS` = 5), listed in
  Files' Applications folder with its own icon: a white globe on a green
  tile, drawn like the others (docs/u3g-design.md).
  - **It is not in the dock at first.** Add to Dock puts it there, as
    with any app.
  - Window slots grow from 8 to 9, so four further windows still fit;
    extra windows now start at slot 5.
- **On opening** it takes the network device, gets an address by DHCP and
  finds the gateway. Then it shows:
  - `Connected  10.0.2.15` (in the function colour), or why not;
  - a dim line with the gateway, the DNS server and the MAC address.
- **Fetching:** type a name or an address and press Enter. It fetches
  `http://<it>/`:
  - a name is looked up first, by DNS over UDP, from the lease's DNS
    server;
  - it shows the address, the status line and the size;
  - then the text, with tags, scripts and styles left out and common
    entities decoded, wrapped at 80 characters.
- **Plain HTTP only:** TLS (HTTPS) is not done, so most of the web today
  will not load. `run` has the open network, so plain-HTTP sites by name
  work there.

## 2. DNS (`zoo_abi::dns`, `zoo_rt::net`)

- **`dns::query`** writes a recursive query for a name's A record.
- **`dns::answer`** reads the reply:
  - it checks the id and the reply flag;
  - it maps error codes (no such name, others);
  - it skips the question and any CNAMEs, following compressed names, to
    the first IPv4 address.
- They are pure functions in `zoo-abi`, so they are unit-tested on the
  host (`cargo test -p zoo-abi`): the exact bytes of a query, a reply with
  a CNAME, a wrong id, NXDOMAIN, a server error, a cut-off reply, and a
  reply with no address.
- **`Stack::udp_ask`** sends a datagram and waits for the reply to our
  port. **`Stack::lookup`** returns a dotted address as it is, else asks
  DNS (twice at most).

## 3. Checks

- **`net-app`** (isolated network, with the harness's HTTP server
  forwarded to 10.0.2.100:80):
  1. Files → Applications → Network;
  2. `Network: connected 10.0.2.15 via 10.0.2.2`;
  3. type `10.0.2.100` and Enter → ``fetched http://10.0.2.100/ … 92
     bytes, `HTTP/1.0 200 OK` ``;
  4. the screenshot shows the window focused, `Connected  10.0.2.15` in
     its colour, and the server's text.
- **The test server waits on its one connection until the case ends.**
  QEMU's `guestfwd` target is a character device: QEMU connects to the
  server when it starts, not when the guest connects. The first version
  gave up after 5 s, before the desktop had even started. It now reads
  200 ms at a time until the case is over.
- **DNS** is checked by the host unit tests. QEMU's isolated network has
  no DNS server, and the open one asks this machine's resolver, so a
  guest test of names would depend on the host's network.

## 4. Not done

- **HTTPS (TLS), redirects,** and a real browser (layout, images).
- **A shared network service.** Only one program may hold the device, so
  Network and the test programs cannot run at the same time.
