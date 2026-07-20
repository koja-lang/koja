#!/usr/bin/env python3
"""Soak test for the shortener example: sustained keep-alive load with
RSS sampling, to catch per-request memory leaks and throughput
regressions that micro-benchmarks miss.

This harness found six distinct compiler/runtime leaks in Jul 2026
(comparison operand temps, arm-scoped bindings, early-return match
subjects, binary-match greedy tails, union-widening ownership, and the
timer-wheel capacity ratchet). A healthy run holds RSS flat after the
first batch; the old failure mode grew ~13 KB per request.

Usage:
  # Terminal 1: start the DB and the server
  cd examples/shortener && docker compose up -d
  koja build --release && ./build/release/shortener

  # Terminal 2: run the soak
  ./benchmarks/shortener_soak.py                      # 40k requests
  ./benchmarks/shortener_soak.py --requests 100000
  ./benchmarks/shortener_soak.py --max-growth-mb 10   # exit 1 on leak

Exit status is non-zero when --max-growth-mb is set and RSS grew more
than that between the first and last sample (the first batch is
excluded so warmup allocations don't count as growth).
"""

import argparse
import json
import socket
import subprocess
import sys
import time


def rss_mb(pid: int) -> float:
    kb = int(subprocess.check_output(["ps", "-o", "rss=", "-p", str(pid)]).strip())
    return kb / 1024


def find_server_pid() -> int:
    out = subprocess.run(["pgrep", "-x", "shortener"], capture_output=True, text=True)
    pids = out.stdout.split()
    if len(pids) != 1:
        sys.exit(
            f"expected exactly one running `shortener` process, found {len(pids)} "
            "(start the example server, or pass --pid)"
        )
    return int(pids[0])


def read_response(sock: socket.socket) -> bytes:
    buf = b""
    while b"\r\n\r\n" not in buf:
        chunk = sock.recv(65536)
        if not chunk:
            raise ConnectionError("server closed the connection mid-response")
        buf += chunk
    head, _, body = buf.partition(b"\r\n\r\n")
    content_length = 0
    for line in head.split(b"\r\n"):
        if line.lower().startswith(b"content-length:"):
            content_length = int(line.split(b":")[1])
    while len(body) < content_length:
        body += sock.recv(65536)
    return body


def create_link(host: str, port: int) -> str:
    payload = json.dumps({"url": "https://example.com/soak"})
    request = (
        f"POST /links HTTP/1.1\r\nHost: {host}\r\n"
        f"Content-Type: application/json\r\nContent-Length: {len(payload)}\r\n"
        f"Connection: close\r\n\r\n{payload}"
    )
    with socket.create_connection((host, port)) as sock:
        sock.sendall(request.encode())
        body = read_response(sock)
    return json.loads(body)["code"]


def run_batch(host: str, port: int, code: str, requests: int, per_connection: int) -> None:
    request = (
        f"GET /{code} HTTP/1.1\r\nHost: {host}\r\nConnection: keep-alive\r\n\r\n"
    ).encode()
    done = 0
    while done < requests:
        with socket.create_connection((host, port)) as sock:
            for _ in range(min(per_connection, requests - done)):
                sock.sendall(request)
                read_response(sock)
                done += 1


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8080)
    parser.add_argument("--pid", type=int, help="server PID (default: pgrep shortener)")
    parser.add_argument("--requests", type=int, default=40_000, help="total GET redirects")
    parser.add_argument("--batches", type=int, default=10, help="RSS samples over the run")
    parser.add_argument(
        "--per-connection", type=int, default=200, help="keep-alive requests per connection"
    )
    parser.add_argument(
        "--max-growth-mb",
        type=float,
        help="fail when RSS grows more than this after the first batch",
    )
    args = parser.parse_args()

    pid = args.pid if args.pid is not None else find_server_pid()
    code = create_link(args.host, args.port)
    batch_size = args.requests // args.batches
    print(f"target={args.host}:{args.port} pid={pid} code={code}")
    print(f"start RSS = {rss_mb(pid):.1f} MB")

    samples = []
    for batch in range(args.batches):
        started = time.time()
        run_batch(args.host, args.port, code, batch_size, args.per_connection)
        elapsed = time.time() - started
        sample = rss_mb(pid)
        samples.append(sample)
        served = batch_size * (batch + 1)
        print(f"after {served:>7} requests: RSS = {sample:5.1f} MB  ({batch_size / elapsed:5.0f} req/s)")

    growth = samples[-1] - samples[0]
    print(f"RSS growth after warmup batch: {growth:+.1f} MB over {args.requests - batch_size} requests")
    if args.max_growth_mb is not None and growth > args.max_growth_mb:
        sys.exit(f"FAIL: RSS grew {growth:.1f} MB > allowed {args.max_growth_mb} MB")


if __name__ == "__main__":
    main()
