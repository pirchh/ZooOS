/* PetrelTest: the home folder as Wine's prefix uses it
 * (docs/linux10d-design.md): names with `:`, brackets and other marks,
 * 200-byte names, symbolic links (relative, to a folder, out to /tmp),
 * renaming a folder that holds a link, *at calls relative to an open home
 * folder, mmap of a home file, utimensat. `petrel-test write` sets it up
 * and checks it; after a reboot of the same disk, `petrel-test check`
 * checks that the names and links were kept. Nothing in it knows about
 * ZooOS. The exit code names the first failure. */
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#define P "/home/user/Petrel"
#define ODD "c: (x86) {4d36e967} +,;=[]!@#$%^&'~"
#define MAPPED 100000

static int fail(int code, const char *what) {
    printf("PetrelTest: FAILED %s (errno %d)\n", what, errno);
    return code;
}

static char long_name[256];

static int put(const char *path, const char *text) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return -1;
    int n = (int)strlen(text);
    int ok = write(fd, text, n) == n;
    return close(fd) == 0 && ok ? 0 : -1;
}

static int get_is(const char *path, const char *want) {
    char buf[128] = {0};
    int fd = open(path, O_RDONLY);
    if (fd < 0) return 0;
    ssize_t n = read(fd, buf, sizeof buf - 1);
    close(fd);
    return n == (ssize_t)strlen(want) && memcmp(buf, want, n) == 0;
}

/* Checks what both boots must see. */
static int check_kept(void) {
    struct stat st;
    if (!get_is(P "/" ODD, "odd")) return fail(40, "odd name");
    char path[512];
    snprintf(path, sizeof path, P "/%s", long_name);
    if (!get_is(path, "long")) return fail(41, "200-byte name");
    char t[128] = {0};
    if (readlink(P "/sub2/link", t, sizeof t) != 13 || strcmp(t, "../target.txt"))
        return fail(42, "readlink");
    if (lstat(P "/sub2/link", &st) || !S_ISLNK(st.st_mode)) return fail(43, "lstat link");
    if (stat(P "/sub2/link", &st) || !S_ISREG(st.st_mode)) return fail(44, "stat through link");
    if (!get_is(P "/sub2/link", "target")) return fail(45, "read through link");
    if (!get_is(P "/dirlink/link", "target")) return fail(46, "through two links");
    /* readdir: names and DT_LNK. */
    DIR *d = opendir(P "/sub2");
    if (!d) return fail(47, "opendir");
    struct dirent *e;
    int links = 0;
    while ((e = readdir(d)))
        if (!strcmp(e->d_name, "link") && e->d_type == DT_LNK) links++;
    closedir(d);
    if (links != 1) return fail(48, "DT_LNK");
    return 0;
}

static int write_all(void) {
    memset(long_name, 'n', 200);
    long_name[200] = 0;
    if (mkdir(P, 0755)) return fail(10, "mkdir");
    if (put(P "/" ODD, "odd")) return fail(11, "odd name");
    char path[512];
    snprintf(path, sizeof path, P "/%s", long_name);
    if (put(path, "long")) return fail(12, "200-byte name");
    errno = 0;
    char longer[300];
    memset(longer, 'x', 256);
    longer[256] = 0;
    snprintf(path, sizeof path, P "/%s", longer);
    if (open(path, O_WRONLY | O_CREAT, 0644) >= 0 || errno != ENAMETOOLONG)
        return fail(13, "256-byte name refused");

    /* Links: relative to the link's folder, to a folder, out of home. */
    if (mkdir(P "/sub", 0755) || put(P "/target.txt", "target")) return fail(20, "set up");
    if (symlink("../target.txt", P "/sub/link")) return fail(21, "symlink");
    errno = 0;
    if (symlink("x", P "/sub/link") == 0 || errno != EEXIST) return fail(22, "symlink EEXIST");
    if (symlink("sub", P "/dirlink")) return fail(23, "symlink to folder");
    if (symlink("/tmp", P "/tmplink")) return fail(24, "symlink out of home");
    if (put(P "/tmplink/petrel.txt", "in tmp") || !get_is("/tmp/petrel.txt", "in tmp"))
        return fail(25, "write through a link to /tmp");
    /* Renaming the folder moves its link. */
    if (rename(P "/sub", P "/sub2")) return fail(26, "rename folder");
    if (unlink(P "/dirlink") || symlink("sub2", P "/dirlink")) return fail(27, "relink");
    errno = 0;
    if (readlink(P "/sub/link", path, sizeof path) >= 0 || errno != ENOENT)
        return fail(28, "old link name gone");
    /* Unlinking a link leaves its target. */
    if (symlink("target.txt", P "/gone") || unlink(P "/gone")) return fail(29, "unlink link");
    if (!get_is(P "/target.txt", "target")) return fail(30, "target kept");

    /* *at calls relative to an open home folder. */
    int dfd = open(P, O_RDONLY | O_DIRECTORY);
    if (dfd < 0) return fail(31, "open folder");
    if (mkdirat(dfd, "at", 0755)) return fail(32, "mkdirat");
    int fd = openat(dfd, "at/f.txt", O_WRONLY | O_CREAT, 0644);
    if (fd < 0 || write(fd, "at", 2) != 2) return fail(33, "openat");
    close(fd);
    struct stat st;
    if (fstatat(dfd, "at/f.txt", &st, 0) || st.st_size != 2) return fail(34, "fstatat");
    if (fstatat(dfd, "sub2/link", &st, AT_SYMLINK_NOFOLLOW) || !S_ISLNK(st.st_mode))
        return fail(35, "fstatat no-follow");
    char t[64] = {0};
    if (readlinkat(dfd, "sub2/link", t, sizeof t) != 13) return fail(36, "readlinkat");
    if (utimensat(dfd, "at/f.txt", NULL, 0)) return fail(37, "utimensat");
    errno = 0;
    if (utimensat(dfd, "at/none", NULL, 0) == 0 || errno != ENOENT) return fail(38, "utimensat ENOENT");
    close(dfd);

    /* mmap of a home file: a private copy of what it holds. */
    static unsigned char data[MAPPED];
    for (int k = 0; k < MAPPED; k++) data[k] = (unsigned char)(k * 31 + 7);
    fd = open(P "/mapped.bin", O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return fail(50, "create mapped.bin");
    for (int done = 0; done < MAPPED;) {
        ssize_t w = write(fd, data + done, MAPPED - done);
        if (w <= 0) return fail(50, "write mapped.bin");
        done += (int)w;
    }
    unsigned char *m = mmap(0, MAPPED, PROT_READ, MAP_PRIVATE, fd, 0);
    if (m == MAP_FAILED || memcmp(m, data, MAPPED)) return fail(51, "mmap whole file");
    unsigned char *at = mmap(0, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 4096 * 20);
    if (at == MAP_FAILED || memcmp(at, data + 4096 * 20, MAPPED - 4096 * 20 < 8192 ? MAPPED - 4096 * 20 : 8192))
        return fail(52, "mmap at an offset");
    at[0] ^= 0xFF;
    unsigned char back = 0;
    if (pread(fd, &back, 1, 4096 * 20) != 1 || back != data[4096 * 20]) return fail(53, "private stays private");
    errno = 0;
    if (mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0) != MAP_FAILED || errno != ENODEV)
        return fail(54, "writable shared mapping refused");
    munmap(m, MAPPED);
    munmap(at, 8192);
    close(fd);

    int r = check_kept();
    if (r) return r;
    printf("PetrelTest: all checks passed\n");
    /* Written and never closed: exit must not lose it (the reboot checks). */
    fd = open(P "/exit.txt", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0 || write(fd, "bye", 3) != 3) return fail(60, "exit.txt");
    return 0;
}

int main(int argc, char **argv) {
    memset(long_name, 'n', 200);
    long_name[200] = 0;
    if (argc == 2 && strcmp(argv[1], "write") == 0) return write_all();
    if (argc == 2 && strcmp(argv[1], "check") == 0) {
        int r = check_kept();
        if (r) return r;
        if (!get_is(P "/exit.txt", "bye")) return fail(61, "write before exit kept");
        printf("PetrelTest: kept across the reboot\n");
        return 0;
    }
    printf("PetrelTest: usage: petrel-test write|check\n");
    return 2;
}
