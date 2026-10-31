# POSIX layer, step 4: writable files

Status: implemented and verified 2026-10-30 (results in docs/verification.md).
Fourth step of strategy A (docs/feasibility/wine-hosting.md). Builds on the
in-memory filesystem and directory capabilities of M5c-1
(docs/m5c1-design.md), which could only create and remove folders.

## 1. Scope

- Create, write (at any offset, growing the file), truncate, remove and
  rename, through directory handles as before (paths relative to a handle,
  no `..`).
- Still in memory: contents are lost at shutdown. Persistence needs a disk
  driver and crash consistency (deferred, see DECISIONS.md).
- Not yet: open-file handles and file descriptors (calls take paths),
  links, timestamps, permissions beyond handle rights, locking.

## 2. Storage

- A seeded file keeps pointing at its initrd bytes until it is first
  written or truncated; then its bytes are copied into pages (copy on
  write). The initrd itself is never written.
- Written files keep their bytes in whole physical pages from the frame
  allocator; the kernel heap (1 MiB) holds only each file's page list.
- Limits: 16 MiB per file (`INVAL` beyond), 16,384 file pages (64 MiB) in
  total and enough free memory, checked before allocating (`NOMEM`).
- Invariant: bytes past a file's length within its last page are zero
  (pages start zeroed; shrinking zeroes the tail), so growing a file,
  writing past its end, or truncating upwards reads zeros.
- Removing a file frees its pages. Node slots are never reused (unchanged
  from M5c-1), so at most 4,096 entries can be created per boot.

## 3. Calls (ABI version 8)

All take a directory handle and an `FsArgs` like calls 20–25.

| # | Call | Right | Notes |
| --- | --- | --- | --- |
| 41 | `file_create(path)` | CREATE | empty file; `EXISTS` if taken |
| 42 | `file_write(path, offset, bytes)` | WRITE (new) | returns bytes written |
| 43 | `file_truncate(path, len)` | WRITE | shrink or zero-extend |
| 44 | `file_remove(path)` | REMOVE | files only (`dir_remove` for folders) |
| 45 | `rename(from, to)` | CREATE + REMOVE | `to` must not exist; a folder cannot move into its own subtree; both paths stay under the handle |

Everything under `/bin` is read-only for every handle (`RIGHTS`).

## 4. Verification

SquirrelHandler, started by Zookeeper with copies of its home and root
handles during the M5c-1 directory checks (`normal` case), checks:

- create, duplicate refused, write and read back;
- a sparse write leaves a zero gap;
- a 100 KiB write read back across page boundaries;
- truncate down and back up (tail reads zero);
- copy-on-write of a seeded file, then restoring it exactly;
- rename of files and folders, refusing overwrite, moving into the
  folder's own subtree, and `..`;
- rights (read-only handle; `/bin`) and the 16 MiB limit;
- cleanup back to the original home.

The boot leak check now counts file pages as free and **audits** that the
file-page counter equals the pages files actually hold.
