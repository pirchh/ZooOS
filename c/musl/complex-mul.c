/* The compiler-runtime helpers musl's libc.so needs for complex
 * multiplication (`__mulsc3`, `__muldc3`, `__mulxc3`), built with it by
 * xtask (docs/linux10-design.md). They follow the algorithm of ISO C
 * Annex G.5.1 (example `_Cmultd`): multiply as real numbers, and if both
 * parts come out NaN, recover the infinities the operands imply.
 *
 * A compiler runtime normally provides these (musl's configure uses -lgcc
 * or -lcompiler_rt). Zig's runtime is a single object that also carries
 * hidden copies of memcpy, memset and strlen; linking it hid musl's own,
 * so they were missing from libc.so's exports. */

/* copysign for the operand's type. */
#define CS(m, s) _Generic((s), float: __builtin_copysignf,     long double: __builtin_copysignl, default: __builtin_copysign)(m, s)

#define COMPLEX_MUL(NAME, T, ONE, ZERO)                                              \
    T _Complex NAME(T a, T b, T c, T d) {                                            \
        T ac = a * c, bd = b * d, ad = a * d, bc = b * c;                            \
        T x = ac - bd, y = ad + bc;                                                  \
        if (__builtin_isnan(x) && __builtin_isnan(y)) {                              \
            int recalc = 0;                                                          \
            if (__builtin_isinf(a) || __builtin_isinf(b)) {                          \
                a = CS(__builtin_isinf(a) ? ONE : ZERO, a);          \
                b = CS(__builtin_isinf(b) ? ONE : ZERO, b);          \
                if (__builtin_isnan(c)) c = CS(ZERO, c);             \
                if (__builtin_isnan(d)) d = CS(ZERO, d);             \
                recalc = 1;                                                          \
            }                                                                        \
            if (__builtin_isinf(c) || __builtin_isinf(d)) {                          \
                c = CS(__builtin_isinf(c) ? ONE : ZERO, c);          \
                d = CS(__builtin_isinf(d) ? ONE : ZERO, d);          \
                if (__builtin_isnan(a)) a = CS(ZERO, a);             \
                if (__builtin_isnan(b)) b = CS(ZERO, b);             \
                recalc = 1;                                                          \
            }                                                                        \
            if (!recalc && (__builtin_isinf(ac) || __builtin_isinf(bd) ||            \
                            __builtin_isinf(ad) || __builtin_isinf(bc))) {           \
                if (__builtin_isnan(a)) a = CS(ZERO, a);             \
                if (__builtin_isnan(b)) b = CS(ZERO, b);             \
                if (__builtin_isnan(c)) c = CS(ZERO, c);             \
                if (__builtin_isnan(d)) d = CS(ZERO, d);             \
                recalc = 1;                                                          \
            }                                                                        \
            if (recalc) {                                                            \
                x = __builtin_inf() * (a * c - b * d);                               \
                y = __builtin_inf() * (a * d + b * c);                               \
            }                                                                        \
        }                                                                            \
        T _Complex r;                                                                \
        __real__ r = x;                                                              \
        __imag__ r = y;                                                              \
        return r;                                                                    \
    }

/* __builtin_copysign takes doubles; float and long double variants by type. */
#define CS(m, s) _Generic((s), float: __builtin_copysignf, \
    long double: __builtin_copysignl, default: __builtin_copysign)(m, s)

/* Hidden: used inside libc.so only, never exported. */
__attribute__((visibility("hidden"))) COMPLEX_MUL(__mulsc3, float, 1.0f, 0.0f)
__attribute__((visibility("hidden"))) COMPLEX_MUL(__muldc3, double, 1.0, 0.0)
__attribute__((visibility("hidden"))) COMPLEX_MUL(__mulxc3, long double, 1.0L, 0.0L)
