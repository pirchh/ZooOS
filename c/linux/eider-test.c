/* EiderTest: a plain Linux program that needs more than printf
 * (docs/linux2-design.md): malloc (brk and mmap underneath), files through
 * stdio and through open/read/lseek/fstat, and the clock with a sleep.
 * Nothing in it knows about ZooOS. The exit code names the first failure. */
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static int fail(int code, const char *what) {
    printf("EiderTest: FAILED %s (errno %d)\n", what, errno);
    return code;
}

int main(void) {
    /* Memory: many small blocks, then one large block, then mmap itself. */
    char *small[200];
    for (int i = 0; i < 200; i++) {
        small[i] = malloc(100 + i);
        if (!small[i]) return fail(10, "small malloc");
        memset(small[i], i, 100 + i);
    }
    for (int i = 0; i < 200; i++) {
        for (int k = 0; k < 100 + i; k++)
            if ((unsigned char)small[i][k] != (unsigned char)i) return fail(11, "small content");
        free(small[i]);
    }
    size_t big = 4u << 20;
    unsigned char *b = malloc(big);
    if (!b) return fail(12, "big malloc");
    for (size_t k = 0; k < big; k += 4096) b[k] = (unsigned char)(k >> 12);
    for (size_t k = 0; k < big; k += 4096)
        if (b[k] != (unsigned char)(k >> 12)) return fail(13, "big content");
    free(b);
    void *m = mmap(0, 65536, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) return fail(14, "mmap");
    memset(m, 0x5a, 65536);
    if (munmap(m, 65536)) return fail(15, "munmap");
    printf("EiderTest: memory ok\n");

    /* Files: stdio line by line, then the raw calls. */
    FILE *f = fopen("/etc/services", "r");
    if (!f) return fail(20, "fopen");
    char line[200];
    int lines = 0;
    while (fgets(line, sizeof line, f)) lines++;
    fclose(f);
    int fd = open("/etc/services", O_RDONLY);
    if (fd < 0) return fail(21, "open");
    struct stat st;
    if (fstat(fd, &st)) return fail(22, "fstat");
    char *all = malloc(st.st_size + 1);
    if (!all) return fail(23, "malloc for the file");
    if (read(fd, all, st.st_size + 1) != st.st_size) return fail(24, "read size");
    if (lseek(fd, 0, SEEK_SET) != 0) return fail(25, "lseek");
    char first;
    if (read(fd, &first, 1) != 1 || first != all[0]) return fail(26, "read after lseek");
    close(fd);
    errno = 0;
    if (open("/etc/no-such-file", O_RDONLY) >= 0 || errno != ENOENT) return fail(27, "ENOENT");
    printf("EiderTest: /etc/services %ld bytes, %d lines\n", (long)st.st_size, lines);

    /* Time: a 200 ms sleep measured on the monotonic clock. */
    struct timespec t0, t1, now;
    if (clock_gettime(CLOCK_MONOTONIC, &t0)) return fail(30, "monotonic clock");
    struct timespec d = {0, 200000000};
    if (nanosleep(&d, 0)) return fail(31, "nanosleep");
    clock_gettime(CLOCK_MONOTONIC, &t1);
    long ms = (t1.tv_sec - t0.tv_sec) * 1000 + (t1.tv_nsec - t0.tv_nsec) / 1000000;
    if (ms < 190) return fail(32, "slept too little");
    if (clock_gettime(CLOCK_REALTIME, &now)) return fail(33, "wall clock");
    printf("EiderTest: slept %ld ms\n", ms);
    printf("EiderTest: unix=%ld\n", (long)now.tv_sec);
    printf("EiderTest: all checks passed\n");
    return 0;
}
