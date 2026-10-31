/* PuffinTest: the first C program on ZooOS (docs/posix5-design.md).
 * Built by `zig cc` for a freestanding target, linked like the Rust
 * programs, packed into the initrd. Exits 0 only if every check passes;
 * the exit code names the first failure. */
#include "zoo.h"

static void say(const char *s) { zoo_log(s, __builtin_strlen(s)); }

/* Appends the decimal or hex form of `v` to `out` at `*at`. */
static void put_u64(char *out, size_t *at, uint64_t v, int hex) {
    char tmp[20];
    int n = 0;
    unsigned base = hex ? 16 : 10;
    do {
        unsigned d = (unsigned)(v % base);
        tmp[n++] = (char)(d < 10 ? '0' + d : 'a' + d - 10);
        v /= base;
    } while (v);
    while (n) out[(*at)++] = tmp[--n];
}

static void put_str(char *out, size_t *at, const char *s) {
    while (*s) out[(*at)++] = *s++;
}

/* Standard CRC-32 (IEEE 802.3, reflected, polynomial 0xEDB88320). */
static uint32_t crc32(const uint8_t *p, size_t n) {
    uint32_t c = 0xFFFFFFFFu;
    for (size_t i = 0; i < n; i++) {
        c ^= p[i];
        for (int k = 0; k < 8; k++) c = (c >> 1) ^ (0xEDB88320u & (0u - (c & 1u)));
    }
    return ~c;
}

/* Newton's method; uses SSE floating point, which the kernel saves per
 * thread. */
static double root(double x) {
    double r = x;
    for (int i = 0; i < 40; i++) r = 0.5 * (r + x / r);
    return r;
}

int main(zoo_handle first, zoo_handle second) {
    (void)first;
    (void)second;
    if (zoo_abi_version() != ZOO_ABI_VERSION) return 10;
    say("PuffinTest: hello from C");

    /* A known CRC-32 check value. */
    static const char fox[] = "The quick brown fox jumps over the lazy dog";
    uint32_t crc = crc32((const uint8_t *)fox, sizeof fox - 1);
    if (crc != 0x414FA339u) return 11;

    /* Floating point: sqrt(2) to 1e-12. */
    double r = root(2.0);
    double err = r * r - 2.0;
    if (err < 0) err = -err;
    if (err > 1e-12) return 12;

    /* Heap memory from the kernel: fill 64 KiB, check it was zeroed and
     * holds what we wrote. */
    int64_t mem = zoo_mem_alloc(64 * 1024);
    if (mem < 0) return 13;
    uint32_t *words = (uint32_t *)(uintptr_t)mem;
    for (size_t i = 0; i < 16 * 1024; i++) {
        if (words[i] != 0) return 13;
        words[i] = (uint32_t)(i * 2654435761u);
    }
    uint32_t mixed = crc32((const uint8_t *)words, 64 * 1024);

    char line[160];
    size_t at = 0;
    put_str(line, &at, "PuffinTest: crc32 0x");
    put_u64(line, &at, crc, 1);
    put_str(line, &at, ", sqrt(2) ok, 64 KiB heap crc32 0x");
    put_u64(line, &at, mixed, 1);
    zoo_log(line, at);
    say("PuffinTest: all C checks passed");
    return 0;
}
