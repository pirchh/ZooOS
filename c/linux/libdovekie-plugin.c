/* libdovekie-plugin.so: loaded at run time with dlopen by DovekieTest
 * (docs/linux8-design.md); it calls back into libdovekie, which the
 * program loaded at start-up. */
int dovekie_add(int a, int b);

int plugin_answer(void) { return 42; }

int plugin_sum(void) { return dovekie_add(40, 2); }
