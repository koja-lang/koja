# Benchmarks

Small, repeatable performance benchmarks for the Koja runtime, with BEAM
(Erlang/OTP) baselines for context. These are micro-benchmarks meant to track
regressions and rough standing versus a mature actor runtime — not a rigorous
cross-language shootout.

## Running

```sh
just bench            # 3 runs each (from the repo root)
RUNS=10 ./run.sh      # more runs for tighter medians
```

`run.sh` builds each Koja program with `--release`, compiles the BEAM baselines
(skipped if `erlc` isn't on `PATH`), runs everything `RUNS` times, and prints a
median comparison table.

## Methodology

Each program brackets **only its workload** with `DateTime.now()` (Koja) /
`erlang:monotonic_time` (BEAM) and prints a `<name>_ms <value>` line. That keeps
VM startup, JIT/compilation, and teardown out of the measurement, so Koja's
ahead-of-time native binary and BEAM's bytecode are compared on the work itself.
The runner reports the median over `RUNS` (and tracks the best internally).

This is deliberately different from wall-clock tools like `hyperfine`, which
time the whole process including startup. `hyperfine` is great for catching
binary/startup regressions; for these sub-second workloads its startup-inclusive
numbers would dominate the signal, so we use the in-workload timing instead.

## Benchmarks

| Program                   | Measures                                                       |
| ------------------------- | -------------------------------------------------------------- |
| `koja/loop.kojs`          | Tight 200M-iteration counting loop — raw integer/branch speed. |
| `koja/recursion.kojs`     | `fib(35)` — recursive call overhead.                           |
| `koja/msg_roundtrip.kojs` | 1M synchronous `call`/reply round-trips to one process.        |
| `koja/spawn_reply.kojs`   | 100k spawn-then-call-then-exit cycles — process churn.         |
| `koja/process_storm.kojs` | 10k processes spawned concurrently, each doing CPU work.       |

BEAM equivalents live in `beam/` (`compute.erl`, `concurrency.erl`,
`storm.erl`) and mirror the same workloads.

## Shortener soak test

`shortener_soak.py` drives the `examples/shortener` server with sustained
keep-alive load while sampling its RSS — the leak-and-throughput regression
check that micro-benchmarks can't provide (it caught six compiler/runtime
memory leaks in Jul 2026). Start the example's compose stack and server, then:

```sh
./shortener_soak.py                                  # 40k requests, RSS per batch
./shortener_soak.py --requests 100000 --batches 20   # longer soak
./shortener_soak.py --max-growth-mb 10               # non-zero exit on growth
```

A healthy run holds RSS flat after the first (warmup) batch. Pair it with
macOS `leaks <pid>` for allocation-site stacks when growth appears.

## Adding a benchmark

1. Add a `koja/<name>.kojs` that prints `"<metric>_ms #{elapsed}"`.
2. Add the program name to `KOJA_BINS` in `run.sh`, and the metric to `LABELS`
   / `NAMES` in the table section.
3. Optionally add a matching `beam/<module>.erl` (module name = file name) and
   list it in `BEAM_MODULES`.
