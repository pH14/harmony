// SPDX-License-Identifier: AGPL-3.0-or-later
#include <errno.h>
#include <inttypes.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#if !defined(__x86_64__) || !defined(__linux__)
#error This corpus requires Linux x86_64
#endif

static void reset(unsigned rounding, unsigned flush) {
    uint16_t cw = (uint16_t)(0x037f | (rounding << 10));
    uint32_t mxcsr = 0x1f80 | (rounding << 13) | (flush ? 0x8040 : 0);
    __asm__ volatile("fninit; fldcw %0; ldmxcsr %1" : : "m"(cw), "m"(mxcsr));
    errno = 0;
}

static void emit(const char *op, unsigned rounding, unsigned flush,
                 const char *input, const void *result, size_t length) {
    uint16_t sw, cw;
    uint32_t mxcsr;
    int error = errno;
    __asm__ volatile("fnstsw %0; fnstcw %1; stmxcsr %2"
                     : "=m"(sw), "=m"(cw), "=m"(mxcsr));
    printf("{\"op\":\"%s\",\"round\":%u,\"flush\":%u,\"input\":\"%s\",\"result\":\"",
           op, rounding, flush, input);
    for (size_t i = 0; i < length; i++) printf("%02x", ((const unsigned char *)result)[i]);
    printf("\",\"x87_status\":%u,\"x87_control\":%u,\"mxcsr\":%u,\"errno\":%d}\n",
           sw, cw, mxcsr, error);
}

static const uint64_t edges64[] = {
    0, 0x8000000000000000, 1, 0x8000000000000001,
    0x000fffffffffffff, 0x0010000000000000, 0x0010000000000001,
    0x3fefffffffffffff, 0x3ff0000000000000, 0x3ff0000000000001,
    0xbff0000000000000, 0x3fdfffffffffffff, 0x3fe0000000000000,
    0x3fe0000000000001, 0x3ff921fb54442d17, 0x3ff921fb54442d18,
    0x3ff921fb54442d19, 0x400921fb54442d18, 0x4340000000000000,
    0x43dfffffffffffff, 0x43e0000000000000, 0x7fefffffffffffff,
    0xffefffffffffffff, 0x7ff0000000000000, 0xfff0000000000000,
    0x7ff8000000000001, 0x7ff0000000000001, 0xfff8000000001234,
    0x40862e42fefa39ef, 0xc0874910d52d3051
};
static const uint32_t edges32[] = {
    0, 0x80000000, 1, 0x80000001, 0x007fffff, 0x00800000,
    0x00800001, 0x3f7fffff, 0x3f800000, 0x3f800001,
    0xbf800000, 0x3effffff, 0x3f000000, 0x3f000001,
    0x40490fda, 0x40490fdb, 0x40490fdc, 0x7f7fffff,
    0xff7fffff, 0x7f800000, 0xff800000, 0x7fc00001, 0x7f800001
};

static uint64_t sample64(size_t i) {
    if (i < sizeof(edges64) / sizeof(edges64[0])) return edges64[i];
    uint64_t x = (uint64_t)i + 0x9e3779b97f4a7c15;
    x = (x ^ (x >> 30)) * 0xbf58476d1ce4e5b9;
    x = (x ^ (x >> 27)) * 0x94d049bb133111eb;
    return x ^ (x >> 31);
}

static void doubles(unsigned rounding, unsigned flush) {
    const char *names[] = {"libm.sin", "libm.cos", "libm.tan", "libm.exp",
                           "libm.log", "libm.sqrt", "libm.log1p", "libm.expm1"};
    double (*volatile functions[])(double) = {sin, cos, tan, exp, log, sqrt, log1p, expm1};
    for (size_t i = 0; i < 94; i++) {
        uint64_t bits = sample64(i);
        double x;
        char input[17];
        memcpy(&x, &bits, sizeof(x));
        snprintf(input, sizeof(input), "%016" PRIx64, bits);
        for (size_t op = 0; op < sizeof(names) / sizeof(names[0]); op++) {
            reset(rounding, flush);
            double result = functions[op](x);
            emit(names[op], rounding, flush, input, &result, sizeof(result));
        }
        for (unsigned op = 0; op < 4; op++) {
            uint64_t magnitude = bits & UINT64_C(0x7fffffffffffffff);
            if (op == 2 && magnitude > UINT64_C(0x3ff0000000000000)) continue;
            if (op == 3 && magnitude > UINT64_C(0x3fd0000000000000)) continue;
            unsigned char result[10];
            reset(rounding, flush);
            switch (op) {
            case 0: __asm__ volatile("fldl %1; fsin; fstpt %0" : "=m"(result) : "m"(x) : "st"); break;
            case 1: __asm__ volatile("fldl %1; fcos; fstpt %0" : "=m"(result) : "m"(x) : "st"); break;
            case 2: __asm__ volatile("fldl %1; f2xm1; fstpt %0" : "=m"(result) : "m"(x) : "st"); break;
            default: __asm__ volatile("fld1; fldl %1; fyl2xp1; fstpt %0" : "=m"(result) : "m"(x) : "st"); break;
            }
            const char *ops[] = {"x87.fsin", "x87.fcos", "x87.f2xm1", "x87.fyl2xp1"};
            emit(ops[op], rounding, flush, input, result, sizeof(result));
        }
    }
}

static void singles(unsigned rounding, unsigned flush) {
    for (size_t i = 0; i < 87; i++) {
        uint32_t bits = i < sizeof(edges32) / sizeof(edges32[0]) ? edges32[i] : (uint32_t)sample64(i + 100);
        float x[4];
        char input[9];
        for (unsigned lane = 0; lane < 4; lane++) memcpy(&x[lane], &bits, sizeof(bits));
        snprintf(input, sizeof(input), "%08" PRIx32, bits);
        for (unsigned op = 0; op < 4; op++) {
            float result[4];
            reset(rounding, flush);
            switch (op) {
            case 0: __asm__ volatile("movups %1, %%xmm0; rcpps %%xmm0, %%xmm0; movups %%xmm0, %0" : "=m"(result) : "m"(x) : "xmm0"); break;
            case 1: __asm__ volatile("movups %1, %%xmm0; rsqrtps %%xmm0, %%xmm0; movups %%xmm0, %0" : "=m"(result) : "m"(x) : "xmm0"); break;
            case 2: __asm__ volatile("movups %1, %%xmm0; sqrtps %%xmm0, %%xmm0; movups %%xmm0, %0" : "=m"(result) : "m"(x) : "xmm0"); break;
            default: {
                float one[4] = {1, 1, 1, 1};
                __asm__ volatile("movups %1, %%xmm0; divps %2, %%xmm0; movups %%xmm0, %0" : "=m"(result) : "m"(one), "m"(x) : "xmm0");
                break;
            }
            }
            const char *ops[] = {"sse.rcpps", "sse.rsqrtps", "sse.sqrtps", "sse.divps"};
            emit(ops[op], rounding, flush, input, result, sizeof(result));
        }
    }
}

int main(void) {
    for (unsigned rounding = 0; rounding < 4; rounding++) {
        for (unsigned flush = 0; flush < 2; flush++) {
            doubles(rounding, flush);
            singles(rounding, flush);
        }
    }
    return ferror(stdout) ? 1 : 0;
}
