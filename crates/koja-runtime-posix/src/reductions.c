#include <stdint.h>

// Per-worker cooperative-preemption budget. It lives in C, not Rust, so it is
// a native thread-local symbol both sides can reach: the runtime seeds it on
// each resume via koja_seed_reductions, and compiled process code decrements
// it inline at every YieldCheck, calling into koja_rt_yield_check only when it
// reaches zero. Stable Rust cannot export a `#[thread_local]` static, hence the
// C translation unit (already compiled alongside the context-switch assembly).
__thread uint32_t koja_reductions_left = 0;

void koja_seed_reductions(uint32_t budget) {
  koja_reductions_left = budget;
}
