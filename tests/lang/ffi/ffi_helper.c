#include <stdint.h>
#include <stdlib.h>
#include <string.h>

int32_t add_c(int32_t a, int32_t b) {
    return a + b;
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
