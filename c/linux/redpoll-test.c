/* RedpollTest: the Linux address-space layout Wine relies on
 * (docs/linux9-design.md). Built like Wine's preloader: no C library, its
 * own entry point and raw system calls, linked at 0x7d7d00000000. It
 * reserves the low 2 GiB as Wine does, maps at 0x400000 (where Windows
 * programs load), carves pages out with munmap, cannot map over itself,
 * uses 4 MiB of stack, and forks with the layout intact. The exit code
 * names the first failure. */
typedef unsigned long u64;
typedef long i64;

static i64 sys6(i64 n, i64 a, i64 b, i64 c, i64 d, i64 e, i64 f) {
    register i64 r10 __asm__("r10") = d;
    register i64 r8 __asm__("r8") = e;
    register i64 r9 __asm__("r9") = f;
    i64 ret;
    __asm__ volatile("syscall"
                     : "=a"(ret)
                     : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return ret;
}

enum { SYS_write = 1, SYS_mmap = 9, SYS_munmap = 11, SYS_fork = 57, SYS_exit = 60, SYS_wait4 = 61 };
enum { PROT_NONE = 0, PROT_READ = 1, PROT_WRITE = 2 };
enum { MAP_PRIVATE = 2, MAP_FIXED = 0x10, MAP_ANON = 0x20, MAP_NORESERVE = 0x4000, MAP_NOREPLACE = 0x100000 };
#define EEXIST 17

static u64 len_of(const char *s) {
    u64 n = 0;
    while (s[n]) n++;
    return n;
}

static void say(const char *s) { sys6(SYS_write, 1, (i64)s, (i64)len_of(s), 0, 0, 0); }

static __attribute__((noreturn)) void quit(int code) {
    for (;;) sys6(SYS_exit, code, 0, 0, 0, 0, 0);
}

static int fail(int code, const char *what) {
    say("RedpollTest: FAILED ");
    say(what);
    say("\n");
    return code;
}

static i64 map(u64 addr, u64 len, int prot, int flags) {
    return sys6(SYS_mmap, (i64)addr, (i64)len, prot, flags, -1, 0);
}

/* Uses `depth` x 64 KiB of stack: each frame passes a pointer to itself
 * down, so every frame stays live (the compiler cannot reuse one). */
static __attribute__((noinline)) u64 deep(int depth, volatile unsigned char *up) {
    volatile unsigned char frame[64 * 1024];
    for (u64 k = 0; k < sizeof frame; k += 4096) frame[k] = (unsigned char)depth;
    u64 below = depth == 0 ? 0 : deep(depth - 1, frame);
    return below + frame[4096] + (up ? up[0] - (unsigned char)(depth + 1) : 0);
}

static int run(void) {
    /* 1. The kernel places mappings high, keeping the low 2 GiB free. */
    i64 any = map(0, 65536, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON);
    if (any < 0 || (u64)any < 0x7f0000000000UL) return fail(10, "kernel-chosen address not high");

    /* 2. Reserve the low 2 GiB as Wine does. */
    u64 lo = 0x10000, lo_len = 0x7fff0000UL - 0x10000;
    if (map(lo, lo_len, PROT_NONE, MAP_PRIVATE | MAP_ANON | MAP_NORESERVE | MAP_NOREPLACE) != (i64)lo)
        return fail(20, "reserving the low 2 GiB");

    /* 3. A Windows-style image address inside it. */
    u64 img = 0x400000, img_len = 0x10000;
    if (map(img, img_len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON | MAP_FIXED) != (i64)img)
        return fail(30, "MAP_FIXED at 0x400000");
    for (u64 k = 0; k < img_len; k += 4096) *(volatile unsigned char *)(img + k) = (unsigned char)(k >> 12);
    if (map(img, 4096, PROT_READ, MAP_PRIVATE | MAP_ANON | MAP_NOREPLACE) != -EEXIST)
        return fail(31, "MAP_FIXED_NOREPLACE over a mapping");

    /* 4. munmap carves one page out; the neighbours keep their data. */
    if (sys6(SYS_munmap, (i64)(img + 0x8000), 4096, 0, 0, 0, 0) != 0) return fail(40, "munmap one page");
    if (map(img + 0x8000, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON | MAP_NOREPLACE) != (i64)(img + 0x8000))
        return fail(41, "mapping the carved page again");
    if (*(volatile unsigned char *)(img + 0x7000) != 7 || *(volatile unsigned char *)(img + 0x9000) != 9 ||
        *(volatile unsigned char *)(img + 0x8000) != 0)
        return fail(42, "neighbouring pages");

    /* 5. The program itself is a mapping too. */
    u64 self = (u64)&run & ~0xfffUL;
    if (self < 0x7d7d00000000UL) return fail(50, "not linked at 0x7d7d00000000");
    if (map(self, 4096, PROT_READ, MAP_PRIVATE | MAP_ANON | MAP_NOREPLACE) != -EEXIST)
        return fail(51, "MAP_FIXED_NOREPLACE over the program");

    /* 6. 4 MiB of stack (committed as used). */
    /* deep(d) adds each depth 0..d; its caller's frame always holds d + 1. */
    if (deep(63, 0) != 63UL * 64 / 2) return fail(60, "deep recursion");

    /* 7. fork: the child has the same layout and data. */
    i64 c = sys6(SYS_fork, 0, 0, 0, 0, 0, 0);
    if (c == 0) quit(*(volatile unsigned char *)(img + 0x9000) == 9 ? 0 : 1);
    int status = -1;
    if (sys6(SYS_wait4, c, (i64)&status, 0, 0, 0, 0) != c || status != 0) return fail(70, "child's layout");

    /* 8. Releasing the low reservation frees it for a new one. */
    if (sys6(SYS_munmap, (i64)lo, (i64)lo_len, 0, 0, 0, 0) != 0) return fail(80, "munmap the low 2 GiB");
    if (map(lo, 65536, PROT_READ, MAP_PRIVATE | MAP_ANON | MAP_NOREPLACE) != (i64)lo)
        return fail(81, "mapping at 0x10000 after release");

    say("RedpollTest: all checks passed\n");
    return 0;
}

/* Entered with a 16-byte aligned stack; `call` keeps the SysV alignment. */
__asm__(".globl _start\n_start:\n  xor %ebp, %ebp\n  and $-16, %rsp\n  call redpoll_main\n  hlt\n");

__attribute__((used)) void redpoll_main(void) { quit(run()); }
