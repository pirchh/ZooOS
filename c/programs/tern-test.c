/* TernTest: process arguments and the clock from C
 * (docs/posix5b1-design.md). Prints its argv and the wall-clock time for
 * the harness to compare with the host, and checks that monotonic time
 * advances across a sleep. The exit code names the first failure. */
#include "zoo.h"

static void put_u64(char *out, size_t *at, uint64_t v) {
    char tmp[20];
    int n = 0;
    do {
        tmp[n++] = (char)('0' + v % 10);
        v /= 10;
    } while (v);
    while (n) out[(*at)++] = tmp[--n];
}

static void say(const char *s) { zoo_log(s, __builtin_strlen(s)); }

static void put_str(char *out, size_t *at, const char *s) {
    while (*s) out[(*at)++] = *s++;
}

int main(zoo_handle first, zoo_handle second) {
    (void)first;
    (void)second;
    if (zoo_abi_version() != ZOO_ABI_VERSION) return 10;

    /* Arguments: split the NUL-separated command line into argv. */
    char cmd[ZOO_MAX_CMDLINE];
    int64_t len = zoo_args_get(cmd, sizeof cmd);
    if (len <= 0 || (uint64_t)len > sizeof cmd) return 11;
    const char *argv[16];
    int argc = 0;
    argv[argc++] = cmd;
    for (int64_t i = 0; i < len && argc < 16; i++) {
        if (cmd[i] == '\0') argv[argc++] = &cmd[i + 1];
    }
    char line[ZOO_MAX_CMDLINE + 40];
    size_t at = 0;
    put_str(line, &at, "TernTest: argc=");
    put_u64(line, &at, (uint64_t)argc);
    put_str(line, &at, " argv=");
    for (int k = 0; k < argc; k++) {
        if (k) line[at++] = ',';
        /* The last argument ends at `len`, not at a NUL. */
        const char *a = argv[k];
        while (a < cmd + len && *a) line[at++] = *a++;
    }
    zoo_log(line, at);

    /* `tern-test wait`: 500 ticks between two lines, for the harness to
     * time against the host clock (docs/h3-design.md). */
    if (argc == 2 && argv[1][0] == 'w' && argv[1][1] == 'a' && argv[1][2] == 'i' &&
        argv[1][3] == 't' && argv[1] + 4 == cmd + len) {
        say("TernTest: waiting 500 ticks");
        zoo_sleep(500);
        say("TernTest: waited");
        return 0;
    }

    /* Wall-clock time, in whole seconds since 1970. */
    int64_t now = zoo_clock(ZOO_CLOCK_REALTIME);
    if (now < 0) return 12;
    at = 0;
    put_str(line, &at, "TernTest: unix=");
    put_u64(line, &at, (uint64_t)now / 1000000000u);
    zoo_log(line, at);

    /* Monotonic time across a 50-tick (0.5 s) sleep. */
    int64_t t0 = zoo_clock(ZOO_CLOCK_MONOTONIC);
    zoo_sleep(50);
    int64_t t1 = zoo_clock(ZOO_CLOCK_MONOTONIC);
    if (t0 < 0 || t1 < t0 + 500000000) return 13;
    at = 0;
    put_str(line, &at, "TernTest: monotonic +");
    put_u64(line, &at, (uint64_t)(t1 - t0) / 1000000u);
    put_str(line, &at, " ms");
    zoo_log(line, at);
    return 0;
}
