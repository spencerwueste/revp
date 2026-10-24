#!/usr/bin/env python3
"""Pretty-print oha's JSON output (read from stdin)."""
import json
import sys

d = json.load(sys.stdin)
s = d["summary"]
lat = d["latencyPercentiles"]
st = d["statusCodeDistribution"]
total = sum(st.values())
rps = s["requestsPerSec"]
print(f"  requests/sec : {rps:>12,.0f}")
print(f"  total        : {total:>12,} reqs, {s['successRate'] * 100:.2f}% success")
print(f"  latency avg  : {s['average'] * 1000:>10.3f} ms")
print(f"  p50 / p90    : {lat['p50'] * 1000:>10.3f} ms / {lat['p90'] * 1000:.3f} ms")
print(f"  p99 / p99.9  : {lat['p99'] * 1000:>10.3f} ms / {lat['p99.9'] * 1000:.3f} ms")
print(f"  slowest      : {s['slowest'] * 1000:>10.3f} ms")
print(f"  status codes : {st}")
print(f"RPS={rps:.0f}")
