/* MurreTest: large and many files in the home folder, across a reboot
 * (docs/linux10c-design.md). `murre-test write` writes them; after a
 * reboot of the same data disk, `murre-test check` reads every byte back.
 * 40 MiB is more than BadgerHandler keeps in memory before it must save,
 * so the write forces saves part-way. Nothing in it knows about ZooOS.
 * The exit code names the first failure. */
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define BIG (40u << 20)
#define CHUNK (64u << 10)
#define SMALL 300
/* The overwritten region: 1 MiB at 17 MiB + 123 (not block aligned). */
#define OVER_AT ((17u << 20) + 123)
#define OVER_LEN (1u << 20)

static unsigned char buf[CHUNK], back[CHUNK];

static int fail(int code, const char *what) {
    printf("MurreTest: FAILED %s (errno %d)\n", what, errno);
    return code;
}

/* The byte at `pos` of big.bin after the overwrite. */
static unsigned char big_byte(unsigned pos) {
    if (pos >= OVER_AT && pos < OVER_AT + OVER_LEN) return (unsigned char)(pos * 7 + 3);
    return (unsigned char)((pos * 2654435761u) >> 13);
}

static unsigned long long now(void) { return __builtin_ia32_rdtsc(); }

static int write_all(void) {
    if (mkdir("/home/user/Murre", 0755)) return fail(10, "mkdir");
    unsigned long long t0 = now();
    int fd = open("/home/user/Murre/big.bin", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return fail(11, "open big");
    for (unsigned at = 0; at < BIG; at += CHUNK) {
        for (unsigned k = 0; k < CHUNK; k++) buf[k] = (unsigned char)(((at + k) * 2654435761u) >> 13);
        if (write(fd, buf, CHUNK) != CHUNK) return fail(12, "write big");
    }
    /* Overwrite a region that does not start on a block boundary. */
    for (unsigned k = 0; k < OVER_LEN; k += CHUNK) {
        for (unsigned j = 0; j < CHUNK; j++) buf[j] = big_byte(OVER_AT + k + j);
        if (pwrite(fd, buf, CHUNK, OVER_AT + k) != CHUNK) return fail(13, "overwrite");
    }
    close(fd);
    unsigned long long t1 = now();
    for (int i = 0; i < SMALL; i++) {
        char name[64];
        snprintf(name, sizeof name, "/home/user/Murre/s%03d.txt", i);
        FILE *f = fopen(name, "w");
        if (!f) return fail(14, "small file");
        for (int k = 0; k < 100 + i; k++) fputc('a' + (i + k) % 26, f);
        if (fclose(f)) return fail(15, "close small");
    }
    if (truncate("/home/user/Murre/s007.txt", 5)) return fail(16, "truncate");
    if (rename("/home/user/Murre/s008.txt", "/home/user/Murre/renamed.txt")) return fail(17, "rename");
    if (unlink("/home/user/Murre/s009.txt")) return fail(18, "unlink");
    printf("MurreTest: wrote 40 MiB in %llu kcycles, %d small files in %llu kcycles\n",
           (t1 - t0) / 1000, SMALL, (now() - t1) / 1000);
    return 0;
}

static int check_all(void) {
    unsigned long long t0 = now();
    int fd = open("/home/user/Murre/big.bin", O_RDONLY);
    if (fd < 0) return fail(30, "open big");
    struct stat st;
    if (fstat(fd, &st) || st.st_size != BIG) return fail(31, "big size");
    for (unsigned at = 0; at < BIG; at += CHUNK) {
        unsigned got = 0;
        while (got < CHUNK) {
            ssize_t r = read(fd, back + got, CHUNK - got);
            if (r <= 0) return fail(32, "read big");
            got += (unsigned)r;
        }
        for (unsigned k = 0; k < CHUNK; k++)
            if (back[k] != big_byte(at + k)) {
                printf("MurreTest: byte %u is %u, not %u\n", at + k, back[k], big_byte(at + k));
                return fail(33, "big content");
            }
    }
    close(fd);
    unsigned long long t1 = now();
    for (int i = 0; i < SMALL; i++) {
        char name[64];
        snprintf(name, sizeof name, "/home/user/Murre/s%03d.txt", i);
        if (i == 8) snprintf(name, sizeof name, "/home/user/Murre/renamed.txt");
        FILE *f = fopen(name, "r");
        if (i == 9) {
            if (f) return fail(34, "unlinked file is back");
            continue;
        }
        if (!f) return fail(35, "small file missing");
        int want = i == 7 ? 5 : 100 + i, n = 0, c;
        while ((c = fgetc(f)) != EOF) {
            if (c != 'a' + (i + n) % 26) return fail(36, "small content");
            n++;
        }
        fclose(f);
        if (n != want) return fail(37, "small size");
    }
    printf("MurreTest: read 40 MiB in %llu kcycles\n", (t1 - t0) / 1000);
    printf("MurreTest: all checks passed\n");
    return 0;
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "write") == 0) return write_all();
    if (argc == 2 && strcmp(argv[1], "check") == 0) return check_all();
    printf("MurreTest: usage: murre-test write|check\n");
    return 2;
}
