/* SkuaTest: a plain Linux program that looks around (docs/linux3-design.md):
 * uname, the environment, directory listing, stat of directories and
 * files, the working directory with relative paths, access and user ids.
 * Nothing in it knows about ZooOS. The exit code names the first failure. */
#define _POSIX_C_SOURCE 200809L
#include <dirent.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/utsname.h>
#include <unistd.h>

static int fail(int code, const char *what) {
    printf("SkuaTest: FAILED %s (errno %d)\n", what, errno);
    return code;
}

static int by_name(const void *a, const void *b) {
    return strcmp(*(char *const *)a, *(char *const *)b);
}

int main(void) {
    struct utsname u;
    if (uname(&u)) return fail(10, "uname");
    printf("SkuaTest: uname %s %s\n", u.sysname, u.machine);

    const char *home = getenv("HOME");
    const char *user = getenv("USER");
    if (!home || strcmp(home, "/home/user") != 0 || !user) return fail(11, "environment");
    printf("SkuaTest: HOME=%s USER=%s\n", home, user);

    /* List the home folder, sorted, without . and .. */
    DIR *d = opendir("/home/user");
    if (!d) return fail(20, "opendir");
    char *names[32];
    int n = 0, dots = 0;
    struct dirent *e;
    while ((e = readdir(d)) && n < 32) {
        if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) {
            dots++;
            continue;
        }
        names[n++] = strdup(e->d_name);
    }
    closedir(d);
    if (dots != 2) return fail(21, "readdir . and ..");
    qsort(names, n, sizeof names[0], by_name);
    char list[256] = "";
    for (int i = 0; i < n; i++) {
        strcat(list, " ");
        strcat(list, names[i]);
    }
    printf("SkuaTest: /home/user:%s\n", list);

    struct stat st;
    if (stat("/home/user/Documents", &st) || !S_ISDIR(st.st_mode)) return fail(22, "stat a folder");
    if (stat("/etc/services", &st) || !S_ISREG(st.st_mode) || st.st_size != 180)
        return fail(23, "stat a file");

    /* The working directory and relative paths. */
    if (chdir("/home/user/Documents")) return fail(30, "chdir");
    char cwd[128];
    if (!getcwd(cwd, sizeof cwd) || strcmp(cwd, "/home/user/Documents") != 0)
        return fail(31, "getcwd");
    FILE *f = fopen("notes.txt", "r");
    if (!f) return fail(32, "relative fopen");
    char line[128] = "";
    if (!fgets(line, sizeof line, f)) return fail(33, "fgets");
    fclose(f);
    line[strcspn(line, "\r\n")] = 0;
    if (chdir("..") || !getcwd(cwd, sizeof cwd) || strcmp(cwd, "/home/user") != 0)
        return fail(34, "chdir ..");
    printf("SkuaTest: cwd %s; Documents/notes.txt starts \"%s\"\n", cwd, line);

    if (access("/etc/services", R_OK)) return fail(40, "access");
    errno = 0;
    if (access("/no/such/file", F_OK) == 0 || errno != ENOENT) return fail(41, "access ENOENT");
    if (getuid() != 1000 || getgid() != 1000) return fail(42, "ids");
    printf("SkuaTest: all checks passed\n");
    return 0;
}
