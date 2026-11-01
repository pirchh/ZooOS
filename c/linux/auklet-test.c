/* AukletTest: a plain Linux program that uses /tmp the way wineserver does
 * (docs/linux10b-design.md): folders, files, symbolic links, *at calls
 * relative to an open folder, files unlinked while open, memfd_create, and
 * MAP_SHARED mappings shared with a forked child. It is started as
 * `linux AUKLET=wing TERM=vt100 auklet-test` and checks those variables.
 * Nothing in it knows about ZooOS. The exit code names the first failure. */
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

static int fail(int code, const char *what) {
    printf("AukletTest: FAILED %s (errno %d)\n", what, errno);
    return code;
}

int main(void) {
    /* Environment from the shell's NAME=VALUE words. */
    const char *a = getenv("AUKLET"), *t = getenv("TERM");
    if (!a || strcmp(a, "wing") != 0) return fail(10, "AUKLET from the command line");
    if (!t || strcmp(t, "vt100") != 0) return fail(11, "TERM replaced");
    int terms = 0;
    for (char **e = environ; *e; e++) terms += strncmp(*e, "TERM=", 5) == 0;
    if (terms != 1 || !getenv("HOME")) return fail(12, "one TERM, HOME kept");

    /* Folders and files. */
    struct stat st;
    if (stat("/tmp", &st) || !S_ISDIR(st.st_mode) || (st.st_mode & 01777) != 01777)
        return fail(20, "/tmp is a sticky folder");
    if (mkdir("/tmp/auklet", 0700)) return fail(21, "mkdir");
    errno = 0;
    if (mkdir("/tmp/auklet", 0700) == 0 || errno != EEXIST) return fail(22, "mkdir EEXIST");
    int fd = open("/tmp/auklet/a", O_RDWR | O_CREAT | O_EXCL, 0600);
    if (fd < 0) return fail(23, "create");
    if (write(fd, "hello tmpfs", 11) != 11) return fail(24, "write");
    char buf[64] = {0};
    if (lseek(fd, 6, SEEK_SET) != 6 || read(fd, buf, sizeof buf) != 5 || strcmp(buf, "tmpfs"))
        return fail(25, "read back");
    if (fstat(fd, &st) || st.st_size != 11 || !S_ISREG(st.st_mode)) return fail(26, "fstat");
    close(fd);

    /* Symbolic links: relative, to a folder, and a loop. */
    if (symlink("a", "/tmp/auklet/link")) return fail(30, "symlink");
    memset(buf, 0, sizeof buf);
    if (readlink("/tmp/auklet/link", buf, sizeof buf) != 1 || buf[0] != 'a') return fail(31, "readlink");
    if (lstat("/tmp/auklet/link", &st) || !S_ISLNK(st.st_mode)) return fail(32, "lstat link");
    if (stat("/tmp/auklet/link", &st) || !S_ISREG(st.st_mode) || st.st_size != 11)
        return fail(33, "stat through link");
    if (symlink("/tmp/auklet", "/tmp/dir-link")) return fail(34, "symlink to folder");
    fd = open("/tmp/dir-link/link", O_RDONLY);
    memset(buf, 0, sizeof buf);
    if (fd < 0 || read(fd, buf, 5) != 5 || strcmp(buf, "hello")) return fail(35, "open through two links");
    close(fd);
    if (symlink("loop", "/tmp/loop")) return fail(36, "symlink loop");
    errno = 0;
    if (open("/tmp/loop", O_RDONLY) >= 0 || errno != ELOOP) return fail(37, "ELOOP");
    unlink("/tmp/loop");

    /* *at calls relative to an open folder. */
    int dfd = open("/tmp/auklet", O_RDONLY | O_DIRECTORY);
    if (dfd < 0) return fail(40, "open folder");
    if (mkdirat(dfd, "sub", 0700)) return fail(41, "mkdirat");
    fd = openat(dfd, "sub/b", O_WRONLY | O_CREAT, 0600);
    if (fd < 0 || write(fd, "b", 1) != 1) return fail(42, "openat");
    close(fd);
    if (fstatat(dfd, "sub/b", &st, 0) || st.st_size != 1) return fail(43, "fstatat");
    if (renameat(dfd, "sub", dfd, "sub2")) return fail(44, "renameat folder");
    if (access("/tmp/auklet/sub2/b", F_OK)) return fail(45, "contents moved with the folder");
    errno = 0;
    if (unlinkat(dfd, "sub2", AT_REMOVEDIR) == 0 || errno != ENOTEMPTY) return fail(46, "ENOTEMPTY");
    if (unlinkat(dfd, "sub2/b", 0) || unlinkat(dfd, "sub2", AT_REMOVEDIR)) return fail(47, "unlinkat");
    close(dfd);
    DIR *d = opendir("/tmp/auklet");
    if (!d) return fail(48, "opendir");
    char list[128] = "";
    struct dirent *e;
    while ((e = readdir(d))) {
        if (e->d_name[0] == '.') continue;
        strcat(list, " ");
        strcat(list, e->d_name);
        if (strcmp(e->d_name, "link") == 0 && e->d_type != DT_LNK) return fail(49, "DT_LNK");
    }
    closedir(d);
    printf("AukletTest: /tmp/auklet:%s\n", list);

    /* An unlinked file lives on while open, then MAP_SHARED with a child. */
    fd = open("/tmp/auklet/shm", O_RDWR | O_CREAT | O_EXCL, 0600);
    if (fd < 0) return fail(50, "create shm");
    if (unlink("/tmp/auklet/shm")) return fail(51, "unlink while open");
    errno = 0;
    if (stat("/tmp/auklet/shm", &st) == 0 || errno != ENOENT) return fail(52, "name gone");
    if (ftruncate(fd, 8192)) return fail(53, "ftruncate");
    char *m = mmap(0, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (m == MAP_FAILED) return fail(54, "mmap shared");
    strcpy(m, "from parent");
    pid_t pid = fork();
    if (pid < 0) return fail(55, "fork");
    if (pid == 0) {
        if (strcmp(m, "from parent") != 0) _exit(1);
        strcpy(m + 4096, "from child");
        _exit(0);
    }
    int status;
    if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status) || WEXITSTATUS(status) != 0)
        return fail(56, "child saw the parent's write");
    if (strcmp(m + 4096, "from child") != 0) return fail(57, "parent sees the child's write");
    memset(buf, 0, sizeof buf);
    if (pread(fd, buf, 10, 4096) != 10 || strcmp(buf, "from child")) return fail(58, "read sees the mapping");
    if (pwrite(fd, "FROM", 4, 0) != 4 || memcmp(m, "FROM parent", 11)) return fail(59, "mapping sees a write");
    /* A private mapping is a copy. */
    char *p = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
    if (p == MAP_FAILED || memcmp(p, "FROM", 4)) return fail(60, "mmap private");
    /* Until written, it shows the file's changes (copy on write; Wine maps
     * read-only views this way). */
    if (pwrite(fd, "Q", 1, 2) != 1 || p[2] != 'Q') return fail(67, "unwritten private sees the file");
    p[0] = 'X';
    if (m[0] != 'F') return fail(61, "private write stays private");
    if (pwrite(fd, "W", 1, 3) != 1 || p[3] != 'M') return fail(68, "written private is a copy");
    munmap(p, 4096);
    /* A private read-only mapping shows later changes to the file (Wine
     * maps read-only views this way), until it is made writable. */
    const char *ro = mmap(0, 4096, PROT_READ, MAP_PRIVATE, fd, 0);
    if (ro == MAP_FAILED) return fail(62, "mmap private read-only");
    if (pwrite(fd, "LATE", 4, 0) != 4 || memcmp(ro, "LATE", 4)) return fail(63, "read-only private sees the file");
    if (mprotect((void *)ro, 4096, PROT_READ | PROT_WRITE)) return fail(64, "mprotect writable");
    ((char *)ro)[0] = 'Y';
    if (m[0] != 'L' || ro[1] != 'A') return fail(65, "writable private is a copy");
    if (pwrite(fd, "Z", 1, 1) != 1 || ro[1] != 'A') return fail(66, "copy no longer follows the file");
    munmap((void *)ro, 4096);
    munmap(m, 8192);
    close(fd);

    /* memfd: two mappings of one file; one goes, the other stays. */
    int mfd = memfd_create("auklet", MFD_CLOEXEC);
    if (mfd < 0) return fail(70, "memfd_create");
    if (ftruncate(mfd, 4096)) return fail(71, "memfd ftruncate");
    char *x = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, mfd, 0);
    char *y = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, mfd, 0);
    if (x == MAP_FAILED || y == MAP_FAILED || x == y) return fail(72, "two mappings");
    strcpy(x, "same page");
    if (strcmp(y, "same page")) return fail(73, "one page, two addresses");
    if (munmap(x, 4096)) return fail(74, "munmap");
    if (strcmp(y, "same page")) return fail(75, "the other mapping stays");
    close(mfd);
    if (strcmp(y, "same page")) return fail(76, "mapping outlives the descriptor");
    munmap(y, 4096);

    if (unlink("/tmp/auklet/link") || unlink("/tmp/auklet/a") || rmdir("/tmp/auklet") || unlink("/tmp/dir-link"))
        return fail(80, "clean up");
    printf("AukletTest: all checks passed\n");
    return 0;
}
