/* LoonTest: a Linux program that never blocks or makes a system call after
 * its first line: the shell must still answer while it runs
 * (docs/linux10b-design.md). It never exits; the test ends with QEMU. */
#include <stdio.h>

int main(void) {
    printf("LoonTest: spinning\n");
    fflush(stdout);
    for (;;) __asm__ volatile("pause");
}
