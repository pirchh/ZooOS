# Wine source

Downloaded, never committed (ignored by Git). The owner chose on
2026-10-31 to build Wine from source (rather than use a prebuilt package).

| Field | Value |
| --- | --- |
| Version | Wine 11.0 (stable) |
| File | `wine-11.0.tar.xz`, 33,172,240 bytes, and `wine-11.0.tar.xz.sign` |
| Source | https://dl.winehq.org/wine/source/11.0/wine-11.0.tar.xz |
| SHA-256 | `c07a6857933c1fc60dff5448d79f39c92481c1e9db5aa628db9d0358446e0701` |
| SHA-512 | `a2c3db14f8cf0d19927466805c8f17c68ee7e93d1196d1162dd2279af497c21ec611a63f7a9de59953bbdfdb44c87d7bf55373c6533224a5d54e434c29428d1b` (equals Alpine `aports` 3.24-stable `community/wine/APKBUILD`, pkgver 11.0) |
| Signature | Good signature from "Alexandre Julliard <julliard@winehq.org>", key `DA23 579A 74D4 AD9A F9D3 F945 CEFA C8EA AF17 519D` (from keys.openpgp.org by fingerprint, checked in a throwaway GnuPG home) |
| Licence | LGPL-2.1-or-later |

It is built for musl in an Alpine Linux environment (tools/alpine/README.md).
