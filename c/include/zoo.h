/* ZooOS system calls for C programs (docs/posix5-design.md). The numbers
 * and constants come from zoo_abi.h, which `cargo xtask` generates from the
 * Rust ABI crate so the two cannot drift apart. */
#ifndef ZOO_H
#define ZOO_H

#include <stddef.h>
#include <stdint.h>
#include "zoo_abi.h"

typedef uint64_t zoo_handle;

static inline int64_t zoo_syscall(uint64_t n, uint64_t a1, uint64_t a2, uint64_t a3) {
    int64_t r;
    __asm__ volatile("int $0x80" : "=a"(r) : "a"(n), "D"(a1), "S"(a2), "d"(a3) : "memory");
    return r;
}

static inline uint64_t zoo_abi_version(void) {
    return (uint64_t)zoo_syscall(ZOO_SYS_ABI_VERSION, 0, 0, 0);
}

__attribute__((noreturn)) static inline void zoo_exit(int code) {
    zoo_syscall(ZOO_SYS_EXIT, (uint64_t)(code & 0xFF), 0, 0);
    for (;;) {}
}

static inline int64_t zoo_log(const char *text, size_t len) {
    return zoo_syscall(ZOO_SYS_LOG, (uint64_t)text, len, 0);
}

/* Returns the start of `len` fresh zeroed bytes, or a negative error. */
static inline int64_t zoo_mem_alloc(uint64_t len) {
    return zoo_syscall(ZOO_SYS_MEM_ALLOC, len, 0, 0);
}

/* Copies up to `len` bytes of the command line (program name, then the
 * arguments, separated by NUL bytes) and returns its full length. */
static inline int64_t zoo_args_get(char *buf, size_t len) {
    return zoo_syscall(ZOO_SYS_ARGS_GET, (uint64_t)buf, len, 0);
}

#define ZOO_CLOCK_REALTIME 0
#define ZOO_CLOCK_MONOTONIC 1

/* Nanoseconds since 1970 (UTC) or since boot, or a negative error. */
static inline int64_t zoo_clock(uint64_t which) {
    return zoo_syscall(ZOO_SYS_CLOCK, which, 0, 0);
}

/* Sleeps for `ticks` timer ticks (10 ms each). */
static inline void zoo_sleep(uint64_t ticks) {
    zoo_syscall(ZOO_SYS_SLEEP, ticks, 0, 0);
}

#endif
