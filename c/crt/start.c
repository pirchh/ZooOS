/* Program entry for C programs on ZooOS (docs/posix5-design.md): the
 * kernel starts a thread at `_start` with the start-up handles in rdi and
 * rsi and rsp+8 16-byte aligned, as at a C function's entry. */
#include "zoo.h"

int main(zoo_handle first, zoo_handle second);

__attribute__((noreturn)) void _start(zoo_handle first, zoo_handle second) {
    zoo_exit(main(first, second));
}
