/* A plain Linux program: nothing in it knows about ZooOS (docs/linux1-design.md).
 * Built with the standard x86_64-linux-musl target; it prints its arguments
 * through printf, so musl's start-up, stdio and exit paths all run. */
#include <stdio.h>

int main(int argc, char **argv) {
    printf("Hello from Linux musl: argc=%d", argc);
    for (int i = 0; i < argc; i++) printf(" argv[%d]=%s", i, argv[i]);
    printf("\n");
    return 0;
}
