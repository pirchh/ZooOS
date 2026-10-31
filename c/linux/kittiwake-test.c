/* KittiwakeTest: a plain Linux program using signal handlers and threads
 * (docs/linux6-design.md): SA_SIGINFO handlers, blocked and pending
 * signals, SIGSEGV recovered with siglongjmp, a SIGILL handler that edits
 * the saved instruction pointer (as Wine does), an alternate signal stack,
 * EINTR and SA_RESTART, pause, pthreads with a mutex and a condition
 * variable, a threaded program killed by a signal, the default action of a
 * fault, and timings. Nothing in it knows about ZooOS. The exit code names
 * the first failure. */
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <linux/futex.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <time.h>
#include <ucontext.h>
#include <unistd.h>

static int fail(int code, const char *what) {
    printf("KittiwakeTest: FAILED %s (errno %d)\n", what, errno);
    return code;
}

static unsigned long long cycles(void) { return __builtin_ia32_rdtsc(); }

static void sleep_ms(long ms) {
    struct timespec t = {ms / 1000, (ms % 1000) * 1000000};
    nanosleep(&t, NULL);
}

static volatile sig_atomic_t got;
static volatile int got_signo, got_code, got_pid;
static volatile void *got_addr;

static void on_info(int sig, siginfo_t *si, void *uc) {
    (void)sig;
    (void)uc;
    got++;
    got_signo = si->si_signo;
    got_code = si->si_code;
    got_pid = si->si_pid;
}

static sigjmp_buf jump;
static void on_segv(int sig, siginfo_t *si, void *uc) {
    (void)sig;
    (void)uc;
    got_addr = si->si_addr;
    got_code = si->si_code;
    siglongjmp(jump, 1);
}

/* Skips the faulting `ud2` (two bytes) by editing the saved context. */
static void on_ill(int sig, siginfo_t *si, void *ucv) {
    (void)sig;
    (void)si;
    ucontext_t *uc = ucv;
    uc->uc_mcontext.gregs[REG_RIP] += 2;
    got++;
}

static char alt[16384];
static volatile int on_alt;
static void on_alt_handler(int sig) {
    (void)sig;
    char here;
    on_alt = &here >= alt && &here < alt + sizeof alt;
}

static void plain(int sig) {
    (void)sig;
    got++;
}

static int catch(int sig, void (*fn)(int, siginfo_t *, void *), int flags) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = fn;
    sa.sa_flags = SA_SIGINFO | flags;
    sigemptyset(&sa.sa_mask);
    return sigaction(sig, &sa, NULL);
}

/* Threads: a shared counter under a mutex, and a condition variable. */
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t ready = PTHREAD_COND_INITIALIZER;
static long counter;
static int flag;

static void *add(void *arg) {
    (void)arg;
    for (int k = 0; k < 20000; k++) {
        pthread_mutex_lock(&lock);
        counter++;
        pthread_mutex_unlock(&lock);
    }
    return (void *)(long)syscall(SYS_gettid);
}

static void *announce(void *arg) {
    (void)arg;
    sleep_ms(20);
    pthread_mutex_lock(&lock);
    flag = 1;
    pthread_cond_signal(&ready);
    pthread_mutex_unlock(&lock);
    return NULL;
}

static void *sleeper(void *arg) {
    (void)arg;
    for (;;) sleep_ms(1000);
    return NULL;
}

static void *nothing(void *arg) { return arg; }

/* A raw clone thread that exits at once, as glibc's threads end: the
 * kernel stores its tid at `ptid`, and on exit clears `ctid` and wakes a
 * futex waiter there (what glibc's pthread_join relies on). */
static long raw_thread(int *ptid, int *ctid) {
    static char stack[4096] __attribute__((aligned(16)));
    unsigned long flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD |
                          CLONE_SYSVSEM | CLONE_PARENT_SETTID | CLONE_CHILD_CLEARTID;
    register long r10 __asm__("r10") = (long)ctid;
    register long r8 __asm__("r8") = 0;
    long ret;
    __asm__ volatile("syscall\n\t"
                     "test %%rax, %%rax\n\t"
                     "jnz 1f\n\t"
                     "mov $60, %%eax\n\t" /* the new thread: exit(0) */
                     "xor %%edi, %%edi\n\t"
                     "syscall\n\t"
                     "1:"
                     : "=a"(ret)
                     : "a"(56L), "D"(flags), "S"(stack + sizeof stack), "d"(ptid), "r"(r10), "r"(r8)
                     : "rcx", "r11", "memory");
    return ret;
}

int main(void) {
    fflush(stdout);
    pid_t me = getpid();
    int status;
    sigset_t set;

    /* 1. A handler with siginfo: raise (tkill) and kill. */
    if (catch(SIGUSR1, on_info, 0)) return fail(10, "sigaction");
    raise(SIGUSR1);
    if (got != 1 || got_signo != SIGUSR1 || got_code != SI_TKILL) return fail(11, "raise");
    kill(me, SIGUSR1);
    if (got != 2 || got_code != SI_USER || got_pid != me) return fail(12, "kill");

    /* 2. Blocked: pending until unblocked. */
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    sigprocmask(SIG_BLOCK, &set, NULL);
    raise(SIGUSR1);
    if (got != 2) return fail(20, "delivered while blocked");
    sigset_t pend;
    sigpending(&pend);
    if (!sigismember(&pend, SIGUSR1)) return fail(21, "sigpending");
    sigprocmask(SIG_UNBLOCK, &set, NULL);
    if (got != 3) return fail(22, "not delivered when unblocked");

    /* 3. SIGSEGV caught twice, left with siglongjmp. */
    if (catch(SIGSEGV, on_segv, 0)) return fail(30, "sigaction SEGV");
    for (int k = 0; k < 2; k++) {
        if (!sigsetjmp(jump, 1)) {
            *(volatile int *)16 = 1;
            return fail(31, "write to 16 did not fault");
        }
        if (got_addr != (void *)16 || got_code != SEGV_MAPERR) return fail(32, "SEGV siginfo");
    }
    sigprocmask(SIG_BLOCK, NULL, &set);
    if (sigismember(&set, SIGSEGV)) return fail(33, "SEGV still blocked after siglongjmp");

    /* 4. SIGILL: the handler moves the saved RIP past `ud2`. */
    if (catch(SIGILL, on_ill, 0)) return fail(40, "sigaction ILL");
    got = 0;
    __asm__ volatile("ud2");
    if (got != 1) return fail(41, "SIGILL resume");

    /* 5. An alternate signal stack. */
    stack_t ss = {.ss_sp = alt, .ss_size = sizeof alt, .ss_flags = 0};
    if (sigaltstack(&ss, NULL)) return fail(50, "sigaltstack");
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_alt_handler;
    sa.sa_flags = SA_ONSTACK;
    sigaction(SIGUSR2, &sa, NULL);
    raise(SIGUSR2);
    if (!on_alt) return fail(51, "handler not on the alternate stack");

    /* 6. EINTR without SA_RESTART, then SA_RESTART. */
    int p[2];
    if (pipe(p)) return fail(60, "pipe");
    struct sigaction pl;
    memset(&pl, 0, sizeof pl);
    pl.sa_handler = plain;
    sigaction(SIGUSR1, &pl, NULL);
    pid_t c = fork();
    if (c == 0) {
        sleep_ms(50);
        kill(getppid(), SIGUSR1);
        sleep_ms(100);
        kill(getppid(), SIGUSR1);
        sleep_ms(50);
        write(p[1], "x", 1);
        _exit(0);
    }
    got = 0;
    char b = 0;
    errno = 0;
    if (read(p[0], &b, 1) != -1 || errno != EINTR || got != 1) return fail(61, "EINTR");
    pl.sa_flags = SA_RESTART;
    sigaction(SIGUSR1, &pl, NULL);
    if (read(p[0], &b, 1) != 1 || b != 'x' || got != 2) return fail(62, "SA_RESTART");
    if (waitpid(c, &status, 0) != c || WEXITSTATUS(status) != 0) return fail(63, "EINTR child");
    close(p[0]);
    close(p[1]);

    /* 7. Threads: 4 x 20000 increments under a mutex; distinct tids. */
    pthread_t th[4];
    for (int k = 0; k < 4; k++)
        if (pthread_create(&th[k], NULL, add, NULL)) return fail(70, "pthread_create");
    long tids[4];
    for (int k = 0; k < 4; k++) {
        void *r;
        if (pthread_join(th[k], &r)) return fail(71, "pthread_join");
        tids[k] = (long)r;
        if (tids[k] == me) return fail(72, "thread tid equals pid");
        for (int j = 0; j < k; j++)
            if (tids[j] == tids[k]) return fail(73, "tids not distinct");
    }
    if (counter != 80000) return fail(74, "counter");

    /* 7b. A raw clone thread: parent-settid, then child-cleartid + wake. */
    static volatile int ptid, ctid;
    ctid = -1;
    long rt = raw_thread((int *)&ptid, (int *)&ctid);
    if (rt <= 0 || ptid != rt) return fail(75, "clone PARENT_SETTID");
    int v;
    while ((v = ctid) != 0) syscall(SYS_futex, &ctid, FUTEX_WAIT, v, NULL, NULL, 0);

    /* 8. A condition variable. */
    pthread_t t;
    pthread_create(&t, NULL, announce, NULL);
    pthread_mutex_lock(&lock);
    while (!flag) pthread_cond_wait(&ready, &lock);
    pthread_mutex_unlock(&lock);
    pthread_join(t, NULL);

    /* 9. A threaded program killed by SIGTERM. */
    if (pipe(p)) return fail(90, "pipe");
    c = fork();
    if (c == 0) {
        pthread_t s;
        pthread_create(&s, NULL, sleeper, NULL);
        write(p[1], "r", 1);
        for (;;) pause();
    }
    read(p[0], &b, 1);
    kill(c, SIGTERM);
    if (waitpid(c, &status, 0) != c || !WIFSIGNALED(status) || WTERMSIG(status) != SIGTERM)
        return fail(91, "threaded child SIGTERM");
    close(p[0]);
    close(p[1]);

    /* 10. A fault without a handler ends the program with its signal. */
    c = fork();
    if (c == 0) {
        signal(SIGILL, SIG_DFL);
        __asm__ volatile("ud2");
        _exit(0);
    }
    if (waitpid(c, &status, 0) != c || !WIFSIGNALED(status) || WTERMSIG(status) != SIGILL)
        return fail(100, "SIGILL default");

    /* 11. pause() returns EINTR after a handler. */
    if (pipe(p)) return fail(110, "pipe");
    c = fork();
    if (c == 0) {
        got = 0;
        write(p[1], "r", 1);
        int r = pause();
        _exit(r == -1 && errno == EINTR && got == 1 ? 0 : 1);
    }
    read(p[0], &b, 1);
    sleep_ms(20);
    kill(c, SIGUSR1);
    if (waitpid(c, &status, 0) != c || WEXITSTATUS(status) != 0) return fail(111, "pause");
    close(p[0]);
    close(p[1]);

    /* 12. Lazily committed memory: 64 MiB costs nothing until touched, and
     * the kernel can write into a page the program never touched. */
    size_t big_len = (size_t)64 << 20;
    char *big = mmap(NULL, big_len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (big == MAP_FAILED) return fail(120, "mmap 64 MiB");
    big[0] = 1;
    big[big_len - 1] = 2;
    if (pipe(p)) return fail(121, "pipe");
    write(p[1], "lazy", 4);
    char *untouched = big + (40 << 20);
    if (read(p[0], untouched, 4) != 4 || memcmp(untouched, "lazy", 4) != 0)
        return fail(122, "read into an untouched page");
    if (big[0] != 1 || big[big_len - 1] != 2 || big[20 << 20] != 0) return fail(123, "lazy contents");
    /* Plain system calls writing into untouched pages. */
    struct timespec *ts = (struct timespec *)(big + (48 << 20));
    if (clock_gettime(CLOCK_MONOTONIC, ts) != 0 || ts->tv_sec < 0) return fail(124, "clock_gettime into an untouched page");
    struct utsname *un = (struct utsname *)(big + (56 << 20));
    if (uname(un) != 0 || strcmp(un->sysname, "Linux") != 0) return fail(125, "uname into an untouched page");
    close(p[0]);
    close(p[1]);
    munmap(big, big_len);

    /* 13. Timings (CPU cycles). */
    catch(SIGUSR1, on_info, 0);
    unsigned long long t0 = cycles();
    for (int k = 0; k < 1000; k++) raise(SIGUSR1);
    unsigned long long t1 = cycles();
    for (int k = 0; k < 50; k++) {
        pthread_create(&t, NULL, nothing, NULL);
        pthread_join(t, NULL);
    }
    unsigned long long t2 = cycles();
    printf("KittiwakeTest: raise+handler %llu cycles, pthread_create+join %llu kcycles (averages)\n",
           (t1 - t0) / 1000, (t2 - t1) / 50 / 1000);
    printf("KittiwakeTest: all checks passed\n");
    return 0;
}
