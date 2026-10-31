/* FulmarTest: a plain Linux program that writes in the home folder
 * (docs/linux4-design.md): mkdir, fopen "w"/"a"/"r", write and read back
 * across many messages, rename, readdir, unlink, ftruncate, relative paths.
 * It leaves /home/user/Fulmar/notes.txt for ZooOS's own tools to read.
 * Nothing in it knows about ZooOS. The exit code names the first failure. */
#define _POSIX_C_SOURCE 200809L
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int fail(int code, const char *what) {
    printf("FulmarTest: FAILED %s (errno %d)\n", what, errno);
    return code;
}

int main(void) {
    if (mkdir("/home/user/Fulmar", 0755)) return fail(10, "mkdir");
    errno = 0;
    if (mkdir("/home/user/Fulmar", 0755) == 0 || errno != EEXIST) return fail(11, "mkdir EEXIST");

    /* Write, append, read back. */
    FILE *f = fopen("/home/user/Fulmar/log.txt", "w");
    if (!f) return fail(20, "fopen w");
    fprintf(f, "line one\nline two\n");
    if (fclose(f)) return fail(21, "fclose w");
    f = fopen("/home/user/Fulmar/log.txt", "a");
    if (!f) return fail(22, "fopen a");
    fprintf(f, "line three\n");
    fclose(f);
    f = fopen("/home/user/Fulmar/log.txt", "r");
    if (!f) return fail(23, "fopen r");
    char text[128];
    size_t n = fread(text, 1, sizeof text - 1, f);
    fclose(f);
    text[n] = 0;
    if (strcmp(text, "line one\nline two\nline three\n") != 0) return fail(24, "read back");
    struct stat st;
    if (stat("/home/user/Fulmar/log.txt", &st) || st.st_size != 29 || !S_ISREG(st.st_mode))
        return fail(25, "stat size");

    /* 3000 bytes with the raw calls: many protocol messages each way. */
    int fd = open("/home/user/Fulmar/big.bin", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return fail(30, "open big");
    unsigned char block[3000];
    for (int i = 0; i < 3000; i++) block[i] = (unsigned char)(i * 7);
    size_t done = 0;
    while (done < sizeof block) {
        ssize_t w = write(fd, block + done, sizeof block - done);
        if (w <= 0) return fail(31, "write big");
        done += (size_t)w;
    }
    close(fd);
    fd = open("/home/user/Fulmar/big.bin", O_RDONLY);
    if (fd < 0) return fail(32, "reopen big");
    unsigned char back[3000];
    done = 0;
    while (done < sizeof back) {
        ssize_t r = read(fd, back + done, sizeof back - done);
        if (r <= 0) return fail(33, "read big");
        done += (size_t)r;
    }
    if (lseek(fd, 0, SEEK_END) != 3000) return fail(34, "lseek end");
    close(fd);
    if (memcmp(block, back, sizeof block) != 0) return fail(35, "big content");

    /* Rename, list, delete, truncate. */
    if (rename("/home/user/Fulmar/log.txt", "/home/user/Fulmar/notes.txt")) return fail(40, "rename");
    errno = 0;
    if (stat("/home/user/Fulmar/log.txt", &st) == 0 || errno != ENOENT) return fail(41, "old name gone");
    DIR *d = opendir("/home/user/Fulmar");
    if (!d) return fail(42, "opendir");
    char list[128] = "";
    struct dirent *e;
    while ((e = readdir(d))) {
        if (e->d_name[0] == '.') continue;
        strcat(list, " ");
        strcat(list, e->d_name);
    }
    closedir(d);
    printf("FulmarTest: /home/user/Fulmar:%s\n", list);
    if (unlink("/home/user/Fulmar/big.bin")) return fail(43, "unlink");
    if (chdir("/home/user/Fulmar")) return fail(44, "chdir");
    fd = open("notes.txt", O_RDWR);
    if (fd < 0) return fail(45, "relative open");
    if (ftruncate(fd, 18)) return fail(46, "ftruncate");
    close(fd);
    if (stat("notes.txt", &st) || st.st_size != 18) return fail(47, "size after ftruncate");

    /* 1 MiB through write and read, timed in CPU cycles (docs/f1-design.md). */
    static unsigned char mib[1 << 20], mib2[1 << 20];
    for (size_t i = 0; i < sizeof mib; i++) mib[i] = (unsigned char)(i * 13 + 5);
    unsigned long long t0 = __builtin_ia32_rdtsc();
    fd = open("big1m.bin", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return fail(50, "open 1 MiB");
    for (done = 0; done < sizeof mib;) {
        ssize_t w = write(fd, mib + done, sizeof mib - done);
        if (w <= 0) return fail(51, "write 1 MiB");
        done += (size_t)w;
    }
    close(fd);
    unsigned long long t1 = __builtin_ia32_rdtsc();
    fd = open("big1m.bin", O_RDONLY);
    if (fd < 0) return fail(52, "reopen 1 MiB");
    for (done = 0; done < sizeof mib2;) {
        ssize_t r = read(fd, mib2 + done, sizeof mib2 - done);
        if (r <= 0) return fail(53, "read 1 MiB");
        done += (size_t)r;
    }
    close(fd);
    unsigned long long t2 = __builtin_ia32_rdtsc();
    if (memcmp(mib, mib2, sizeof mib) != 0) return fail(54, "1 MiB content");
    unlink("big1m.bin");
    printf("FulmarTest: 1 MiB write %llu kcycles, read %llu kcycles\n", (t1 - t0) / 1000, (t2 - t1) / 1000);
    printf("FulmarTest: all checks passed\n");
    return 0;
}
