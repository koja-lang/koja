#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

int32_t add_c(int32_t a, int32_t b) {
    return a + b;
}

/* One function per FFI-admissible width, so both backends prove they
 * marshal every type the typechecker lets through an extern boundary.
 * The narrow integer returns are negative or above the signed range
 * on purpose, so a caller that reads the wrong width or extends with
 * the wrong signedness prints the wrong number. */

int8_t negate_i8(int8_t x) {
    return (int8_t)-x;
}

uint8_t max_u8(uint8_t x) {
    return (uint8_t)(x | 0x80);
}

int16_t negate_i16(int16_t x) {
    return (int16_t)-x;
}

uint16_t max_u16(uint16_t x) {
    return (uint16_t)(x | 0x8000);
}

uint32_t max_u32(uint32_t x) {
    return x | 0x80000000u;
}

int64_t negate_i64(int64_t x) {
    return -x;
}

/* `Int64` has no `print`, so an `Int64` result comes back through
 * here to be printed as an `Int32`. */
int32_t low_i32(int64_t x) {
    return (int32_t)x;
}

uint64_t shift_u64(uint64_t x) {
    return x << 60;
}

bool is_even(int32_t x) {
    return (x & 1) == 0;
}

bool flip(bool x) {
    return !x;
}

float halve_f32(float x) {
    return x / 2.0f;
}

double half_c(double x) {
    return x / 2.0;
}

double nan_c(void) {
    return 0.0 / 0.0;
}

static double nan_cell;

double *nan_ptr_c(void) {
    nan_cell = 0.0 / 0.0;
    return &nan_cell;
}

int32_t mul_c(int32_t a, int32_t b) {
    return a * b;
}

void fill_array(int32_t *buf, int32_t count, int32_t value) {
    for (int32_t i = 0; i < count; i++) {
        buf[i] = value + i;
    }
}

int32_t sum_array(const int32_t *buf, int32_t count) {
    int32_t total = 0;
    for (int32_t i = 0; i < count; i++) {
        total += buf[i];
    }
    return total;
}

int32_t read_at(const int32_t *buf, int32_t index) {
    return buf[index];
}

uint8_t *make_greeting(const uint8_t *name, int64_t name_len) {
    const char *prefix = "Hello, ";
    const char *suffix = "!";
    int64_t prefix_len = 7;
    int64_t suffix_len = 1;
    int64_t total = prefix_len + name_len + suffix_len + 1;
    uint8_t *buf = (uint8_t *)malloc(total);
    memcpy(buf, prefix, prefix_len);
    memcpy(buf + prefix_len, name, name_len);
    memcpy(buf + prefix_len + name_len, suffix, suffix_len);
    buf[total - 1] = '\0';
    return buf;
}
