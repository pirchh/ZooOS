/* DovekieTest: a dynamically linked Linux program (docs/linux8-design.md).
 * The kernel starts musl's dynamic loader (/lib/ld-musl-x86_64.so.1),
 * which maps libdovekie.so, relocates everything and runs constructors;
 * the program then uses the library's functions, globals and thread-local
 * storage, loads a plugin with dlopen, and times starting a static and a
 * dynamic program. Nothing in it knows about ZooOS. The exit code names
 * the first failure. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <pthread.h>
#include <spawn.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;
extern _Thread_local int dovekie_tls;
extern int dovekie_ready;
extern const char *dovekie_greeting;
int dovekie_add(int a, int b);
int dovekie_bump(void);
int dovekie_format(char *out, unsigned n, int v);

static int fail(int code, const char *what) {
    printf("DovekieTest: FAILED %s\n", what);
    return code;
}

static unsigned long long cycles(void) { return __builtin_ia32_rdtsc(); }

static void *bump_in_thread(void *arg) {
    (void)arg;
    return (void *)(long)dovekie_bump();
}

/* Average cycles to start `path` (output to a pipe) and wait for it. */
static unsigned long long spawn_cost(const char *path, int runs) {
    int p[2];
    if (pipe(p)) return 0;
    posix_spawn_file_actions_t fa;
    posix_spawn_file_actions_init(&fa);
    posix_spawn_file_actions_adddup2(&fa, p[1], 1);
    char *argv[] = {"hello", NULL};
    unsigned long long t0 = cycles();
    for (int k = 0; k < runs; k++) {
        pid_t c;
        int status;
        if (posix_spawn(&c, path, &fa, NULL, argv, environ)) return 0;
        waitpid(c, &status, 0);
        if (WEXITSTATUS(status) != 0) return 0;
    }
    unsigned long long t1 = cycles();
    close(p[0]);
    close(p[1]);
    return (t1 - t0) / (unsigned long long)runs;
}

int main(void) {
    fflush(stdout);

    /* 1. Functions, globals and a constructor from libdovekie.so. */
    if (dovekie_add(2, 3) != 5) return fail(10, "dovekie_add");
    if (dovekie_ready != 42) return fail(11, "library constructor");
    if (strcmp(dovekie_greeting, "hello from libdovekie") != 0) return fail(12, "library global");
    printf("DovekieTest: libdovekie says: %s\n", dovekie_greeting);

    /* 2. Thread-local storage in the library: each thread has its own. */
    if (dovekie_bump() != 8 || dovekie_bump() != 9) return fail(20, "library TLS");
    pthread_t t;
    void *r;
    if (pthread_create(&t, NULL, bump_in_thread, NULL) || pthread_join(t, &r)) return fail(21, "thread");
    if ((long)r != 8 || dovekie_tls != 9) return fail(22, "TLS per thread");

    /* 3. The library calling libc. */
    char buf[32];
    if (dovekie_format(buf, sizeof buf, 5) != 7 || strcmp(buf, "value=5") != 0) return fail(30, "library snprintf");

    /* 4. dlopen a plugin that calls back into libdovekie. */
    void *h = dlopen("libdovekie-plugin.so", RTLD_NOW);
    if (!h) return fail(40, dlerror());
    int (*answer)(void) = (int (*)(void))dlsym(h, "plugin_answer");
    int (*sum)(void) = (int (*)(void))dlsym(h, "plugin_sum");
    if (!answer || !sum || answer() != 42 || sum() != 42) return fail(41, "plugin symbols");
    if (dlopen("libno-such-library.so", RTLD_NOW) != NULL || dlerror() == NULL) return fail(42, "missing library");

    /* 5. Start-up cost: a static and a dynamic program. */
    unsigned long long s = spawn_cost("/linux/hello", 10);
    unsigned long long d = spawn_cost("/linux/hello-dyn", 10);
    if (!s || !d) return fail(50, "spawn");
    printf("DovekieTest: start+wait static %llu kcycles, dynamic %llu kcycles (averages)\n", s / 1000, d / 1000);
    printf("DovekieTest: all checks passed\n");
    return 0;
}
