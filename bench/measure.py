#!/usr/bin/env python3
"""Reduce oha JSON (stdin) to: rps p50_ms p99_ms pct_200."""
import json
import sys

d = json.load(sys.stdin)
s = d["summary"]
lat = d["latencyPercentiles"]
st = d["statusCodeDistribution"]
total = sum(st.values())
ok = st.get("200", 0)
pct = ok / total * 100 if total else 0.0
print(f"{s['requestsPerSec']:.0f} {lat['p50'] * 1000:.3f} {lat['p99'] * 1000:.3f} {pct:.1f}")
