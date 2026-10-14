#!/usr/bin/env python3
"""Summarizes a SKIFF_TRACE keystroke latency file.

    python3 scripts/latency.py /tmp/skiff-trace.jsonl [--since-min N]

Each stage gets count, p50, p90, p99 and max in ms, split into keys typed
into a quiet session and keys typed while the session was printing, then
by system load band (low, medium, high: CPU or IO pressure on Linux, the
load average per core elsewhere). Then the slowest main-thread stalls with
the steps that ran in them, and slow async work.

`echo` is split into daemon-in (to skiffd's PTY write), program (the PTY
write to the first output read) and daemon-out (that read to the page).
Files from before the split show `echo` instead.
"""
import json
import sys
import time

SPLIT = ["daemon-in", "program", "daemon-out"]
BANDS = ["low", "medium", "high", "unknown"]


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(p / 100 * len(xs)))]


def stages(rows):
    split = any(r.get("program") is not None for r in rows)
    return ["queue", "ipc"] + (SPLIT if split else ["echo"]) + ["parse", "render", "total"]


def table(rows, title):
    print(f"\n{title}: {len(rows)} keys")
    print(f"  {'stage':<10} {'n':>5} {'p50':>7} {'p90':>7} {'p99':>7} {'max':>7}")
    for s in stages(rows):
        xs = [r[s] for r in rows if r.get(s) is not None]
        if not xs:
            print(f"  {s:<10} {0:>5}")
            continue
        print(f"  {s:<10} {len(xs):>5} {pct(xs, 50):>7.2f} {pct(xs, 90):>7.2f} {pct(xs, 99):>7.2f} {max(xs):>7.2f}")
    missing = sum(1 for r in rows if r.get("echo") is None)
    if missing:
        print(f"  no echo within 1 s: {missing}")


def main():
    args = sys.argv[1:]
    if not args:
        sys.exit(__doc__)
    since = None
    if "--since-min" in args:
        i = args.index("--since-min")
        since = (time.time() - float(args[i + 1]) * 60) * 1000
        del args[i : i + 2]
    lines = [json.loads(l) for l in open(args[0]) if l.strip()]
    if since:
        lines = [r for r in lines if r["at"] >= since]
    builds = sorted({r["build"] for r in lines if r.get("build")})
    if builds:
        print("Builds: " + "; ".join(builds))
    rows = [r for r in lines if "busy" in r]
    table([r for r in rows if not r["busy"]], "Quiet session")
    table([r for r in rows if r["busy"]], "Session printing")
    for b in BANDS:
        rs = [r for r in rows if r.get("band") == b]
        if rs:
            table(rs, f"Load {b}")
    if len(builds) > 1:
        for b in builds:
            table([r for r in rows if r.get("build") == b], f"Build {b}")
    stalls = [r for r in lines if "stall" in r]
    if stalls:
        print(f"\nMain-thread stalls over 50 ms: {len(stalls)}")
        for st in sorted(stalls, key=lambda r: -r["stall"])[:10]:
            steps = ", ".join(f"{s['name']} {s['ms']}" for s in st["steps"]) or "no step recorded"
            print(f"  {st['stall']:>7.1f} ms  {st.get('band', ''):<7} {steps}")
    works = [r for r in lines if "work" in r]
    if works:
        print(f"\nSlow work over 50 ms: {len(works)}")
        for w in sorted(works, key=lambda r: -r["ms"])[:10]:
            print(f"  {w['ms']:>7.1f} ms  {w.get('band', ''):<7} {w['work']}")
    # A WebGL renderer attached to a pane, against a bare context.
    gls = [r["gl"] for r in lines if "gl" in r]
    probes = [r["glprobe"] for r in lines if "glprobe" in r]
    if gls:
        ctor = sorted(g["ctor"] for g in gls)
        load = sorted(g["load"] for g in gls)
        mid = len(gls) // 2
        warm = sum(1 for g in gls if g.get("warm"))
        print(f"\nWebGL attach ({len(gls)}, {warm} warmed in idle frames): addon {ctor[mid]:.1f} ms, loadAddon {load[mid]:.1f} ms median, {load[-1]:.1f} ms max", end="")
        print(f"; bare context {sorted(probes)[len(probes) // 2]:.1f} ms" if probes else "")


if __name__ == "__main__":
    main()
