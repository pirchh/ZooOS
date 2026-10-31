/* GuillemotTest: a plain Linux program that starts other programs
 * (docs/linux5-design.md): fork and wait, separate memory, pipes, dup2 and
 * execvp, posix_spawn (musl's vfork-style clone), kill, SIGPIPE, a child
 * writing in the home folder, and timings. Nothing in it knows about
 * ZooOS. The exit code names the first failure. */
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

extern char **environ;

static int fail(int code, const char *what) {
    printf("GuillemotTest: FAILED %s (errno %d)\n", what, errno);
    return code;
}

static unsigned long long cycles(void) { return __builtin_ia32_rdtsc(); }

/* Reads a pipe to its end into buf; the length. */
static size_t drain(int fd, char *buf, size_t cap) {
    size_t n = 0;
    for (;;) {
        ssize_t r = read(fd, buf + n, cap - 1 - n);
        if (r <= 0) break;
        n += (size_t)r;
        if (n == cap - 1) break;
    }
    buf[n] = 0;
    return n;
}

static int shared_value = 1;

int main(void) {
    fflush(stdout);
    pid_t me = getpid();
    int status;

    /* 1. fork, exit code, getppid. */
    pid_t c = fork();
    if (c < 0) return fail(10, "fork");
    if (c == 0) _exit(getppid() == me ? 7 : 8);
    if (waitpid(c, &status, 0) != c) return fail(11, "waitpid");
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 7) return fail(12, "child exit code");
    errno = 0;
    if (waitpid(-1, &status, 0) != -1 || errno != ECHILD) return fail(13, "ECHILD");

    /* 2. Separate memory. */
    c = fork();
    if (c == 0) {
        shared_value = 2;
        _exit(shared_value == 2 ? 0 : 1);
    }
    if (waitpid(c, &status, 0) != c || WEXITSTATUS(status) != 0) return fail(20, "child memory");
    if (shared_value != 1) return fail(21, "parent memory changed");

    /* 3. A pipe from a child; the end of data once it exits. */
    int p[2];
    char buf[4096];
    if (pipe(p)) return fail(30, "pipe");
    c = fork();
    if (c == 0) {
        close(p[0]);
        const char *m = "through a pipe";
        _exit(write(p[1], m, strlen(m)) == (ssize_t)strlen(m) ? 0 : 1);
    }
    close(p[1]);
    drain(p[0], buf, sizeof buf);
    close(p[0]);
    waitpid(c, &status, 0);
    if (strcmp(buf, "through a pipe") != 0) return fail(31, "pipe data");

    /* 4. dup2 + execvp (PATH search) with the output captured. */
    if (pipe(p)) return fail(40, "pipe");
    c = fork();
    if (c == 0) {
        dup2(p[1], 1);
        close(p[0]);
        close(p[1]);
        char *argv[] = {"hello", "from-guillemot", NULL};
        execvp("hello", argv);
        _exit(127);
    }
    close(p[1]);
    drain(p[0], buf, sizeof buf);
    close(p[0]);
    if (waitpid(c, &status, 0) != c || WEXITSTATUS(status) != 0) return fail(41, "exec child");
    if (!strstr(buf, "argc=2 argv[0]=hello argv[1]=from-guillemot")) return fail(42, "exec output");
    printf("GuillemotTest: exec output: %s", buf);

    /* 5. posix_spawn with a file action, and a missing program. */
    if (pipe(p)) return fail(50, "pipe");
    posix_spawn_file_actions_t fa;
    posix_spawn_file_actions_init(&fa);
    posix_spawn_file_actions_adddup2(&fa, p[1], 1);
    posix_spawn_file_actions_addclose(&fa, p[0]);
    char *sargv[] = {"hello", "spawned", NULL};
    if (posix_spawn(&c, "/linux/hello", &fa, NULL, sargv, environ)) return fail(51, "posix_spawn");
    close(p[1]);
    drain(p[0], buf, sizeof buf);
    close(p[0]);
    if (waitpid(c, &status, 0) != c || WEXITSTATUS(status) != 0) return fail(52, "spawn child");
    if (!strstr(buf, "argv[1]=spawned")) return fail(53, "spawn output");
    int e = posix_spawn(&c, "/linux/no-such-program", NULL, NULL, sargv, environ);
    if (e != ENOENT) {
        errno = e;
        return fail(54, "spawn of a missing program");
    }

    /* 6. WNOHANG, then kill with SIGTERM. */
    c = fork();
    if (c == 0) {
        struct timespec t = {30, 0};
        nanosleep(&t, NULL);
        _exit(0);
    }
    if (waitpid(c, &status, WNOHANG) != 0) return fail(60, "WNOHANG");
    if (kill(c, SIGTERM)) return fail(61, "kill");
    if (waitpid(c, &status, 0) != c) return fail(62, "wait killed");
    if (!WIFSIGNALED(status) || WTERMSIG(status) != SIGTERM) return fail(63, "killed status");

    /* 7. SIGPIPE: ends a child by default; ignored, the write fails. */
    if (pipe(p)) return fail(70, "pipe");
    close(p[0]);
    c = fork();
    if (c == 0) {
        write(p[1], "x", 1);
        _exit(0);
    }
    waitpid(c, &status, 0);
    if (!WIFSIGNALED(status) || WTERMSIG(status) != SIGPIPE) return fail(71, "SIGPIPE default");
    signal(SIGPIPE, SIG_IGN);
    errno = 0;
    if (write(p[1], "x", 1) != -1 || errno != EPIPE) return fail(72, "EPIPE");
    close(p[1]);

    /* 8. 256 KiB through a 64 KiB pipe: the writer waits for the reader. */
    if (pipe(p)) return fail(80, "pipe");
    c = fork();
    if (c == 0) {
        close(p[1]);
        unsigned long sum = 0, n = 0;
        ssize_t r;
        while ((r = read(p[0], buf, sizeof buf)) > 0)
            for (ssize_t k = 0; k < r; k++, n++) sum += (unsigned char)buf[k] ^ (n & 0xff);
        _exit(n == 256 * 1024 && sum == 0 ? 0 : 1);
    }
    close(p[0]);
    static char big[256 * 1024];
    for (size_t k = 0; k < sizeof big; k++) big[k] = (char)(k & 0xff);
    for (size_t done = 0; done < sizeof big;) {
        ssize_t w = write(p[1], big + done, sizeof big - done);
        if (w <= 0) return fail(81, "big write");
        done += (size_t)w;
    }
    close(p[1]);
    if (waitpid(c, &status, 0) != c || WEXITSTATUS(status) != 0) return fail(82, "big read");

    /* 9. A child writes in the home folder; the parent reads it. */
    c = fork();
    if (c == 0) {
        FILE *f = fopen("/home/user/guillemot.txt", "w");
        if (!f) _exit(1);
        fputs("written by the child\n", f);
        _exit(fclose(f) ? 2 : 0);
    }
    if (waitpid(c, &status, 0) != c || WEXITSTATUS(status) != 0) return fail(90, "child home write");
    FILE *f = fopen("/home/user/guillemot.txt", "r");
    if (!f || !fgets(buf, sizeof buf, f)) return fail(91, "read child's file");
    fclose(f);
    if (strcmp(buf, "written by the child\n") != 0) return fail(92, "child's file content");

    /* 10. Timings (CPU cycles). */
    unsigned long long t0 = cycles();
    for (int k = 0; k < 50; k++) {
        c = fork();
        if (c == 0) _exit(0);
        waitpid(c, &status, 0);
    }
    unsigned long long t1 = cycles();
    char *mem4 = malloc(4 << 20);
    if (!mem4) return fail(100, "malloc");
    memset(mem4, 1, 4 << 20);
    unsigned long long t2 = cycles();
    for (int k = 0; k < 10; k++) {
        c = fork();
        if (c == 0) _exit(0);
        waitpid(c, &status, 0);
    }
    unsigned long long t3 = cycles();
    if (pipe(p)) return fail(101, "pipe");
    posix_spawn_file_actions_init(&fa);
    posix_spawn_file_actions_adddup2(&fa, p[1], 1);
    unsigned long long t4 = cycles();
    for (int k = 0; k < 50; k++) {
        if (posix_spawn(&c, "/linux/hello", &fa, NULL, sargv, environ)) return fail(102, "spawn loop");
        waitpid(c, &status, 0);
    }
    unsigned long long t5 = cycles();
    printf("GuillemotTest: fork+wait %llu kcycles, with 4 MiB touched %llu kcycles, "
           "posix_spawn+wait %llu kcycles (averages)\n",
           (t1 - t0) / 50 / 1000, (t3 - t2) / 10 / 1000, (t5 - t4) / 50 / 1000);
    printf("GuillemotTest: all checks passed\n");
    return 0;
}
