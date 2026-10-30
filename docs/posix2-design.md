# POSIX layer, step 2: address-space reservations and protection

Status: implemented and verified 2026-10-30 (results in docs/verification.md).
Second step of strategy A (docs/feasibility/wine-hosting.md). Wine builds
the Windows address space itself: it reserves large ranges, commits pages
inside them, changes protection (PE sections, JIT code) and frees them
(`VirtualAlloc`, `VirtualProtect`, `VirtualFree`, `VirtualQuery`). A POSIX
`mmap`/`mprotect`/`munmap` layer can later be built on the same calls.

## 1. Scope

- In this step: anonymous memory only; reserve, commit (zeroed pages),
  protect, decommit, release and query; read, write and execute protection.
- Not yet: no-access pages (guard pages) and demand paging, which need user
  exception delivery (step 3); file-backed and shared mappings; copy-on-write.

## 2. Window and limits

- `vm_*` calls work only in `[0x4000_0000, 0x1000_0000_0000)`: above the
  program image (`0x20_0000`–`0x4000_0000`) and below the heap
  (`mem_alloc`). Programs keep their existing image, stack, heap and
  file-mapping regions.
- Addresses and lengths are page-aligned (4 KiB). Kernel-chosen
  reservations are first-fit and 64 KiB-aligned, as on Windows.
- ≤ 256 reservations and ≤ 1 GiB committed per process. The commit limit
  and free physical memory are checked **before** any page is allocated, so
  an over-limit commit changes nothing.
- Known gap for Wine: Windows programs usually expect to load at
  `0x400000`, inside ZooOS's program-image region. Moving the ZooOS image
  (or the window) is left for when PE loading starts.

## 3. State lives in the page tables

The kernel heap is 1 MiB, too small for per-page records of large commits.
A process keeps only its reservation list `(start, len)`; whether a page is
committed and its protection are read from its page-table entry. A walker
(`next_mapped`) skips empty page-table subtrees, so releasing a 4 TiB
reservation with a few committed pages is quick. Committed pages are freed
with the address space when the process ends.

## 4. System calls (ABI version 6)

| # | Call | Notes |
| --- | --- | --- |
| 32 | `vm_reserve(addr, len)` | `addr` 0: kernel chooses; fixed ranges must be free (`EXISTS`) and in the window (`INVAL`) |
| 33 | `vm_commit(addr, len, prot)` | range inside one reservation (`NOTFOUND` otherwise); new pages zeroed, existing ones keep their contents; all get `prot` |
| 34 | `vm_protect(addr, len, prot)` | every page must be committed (`NOTFOUND`) |
| 35 | `vm_decommit(addr, len)` | frees the pages; the range stays reserved |
| 36 | `vm_release(base)` | frees and unreserves a whole reservation by its base |
| 37 | `vm_query(addr, out)` | `VmInfo`: the run of pages from `addr` with one state (free, reserved, committed) and protection, plus the enclosing reservation |

`prot` is `READ` (required), `WRITE`, `EXEC`. Write+execute is allowed
(JIT compilers need it).

## 5. Verification

MoleHandler, run by Zookeeper at boot (`normal` case), checks: first
reservation at the window start; fixed placement; overlap, out-of-window
and unaligned requests refused; a 4 TiB reserve and release; commit and a
memory round trip; query results for committed and reserved runs; errors
for commits outside reservations, protecting uncommitted pages and
protection without read; protection splitting a run; generated code
running only after `R+X`; decommitted memory reading back as zero after
recommit; the commit limit refused up front; release and double release.

Four child processes must crash with exactly these faults (the verifier
matches the kernel's crash lines): reading a reserved page (`read`,
`not-present`), writing a read-only page (`write`, `protection`),
executing a non-executable page (`execute`, `protection`), writing a
released page (`write`, `not-present`). `TEST process-cleanup ok` must
still hold.
