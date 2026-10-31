/* libdovekie.so: a shared library for DovekieTest (docs/linux8-design.md):
 * a constructor, an exported global, thread-local storage, and calls into
 * libc from inside a library. */
#include <stdio.h>

_Thread_local int dovekie_tls = 7;
int dovekie_ready;
const char *dovekie_greeting = "hello from libdovekie";

__attribute__((constructor)) static void dovekie_init(void) { dovekie_ready = 42; }

int dovekie_add(int a, int b) { return a + b; }

int dovekie_bump(void) { return ++dovekie_tls; }

int dovekie_format(char *out, unsigned n, int v) { return snprintf(out, n, "value=%d", v); }
