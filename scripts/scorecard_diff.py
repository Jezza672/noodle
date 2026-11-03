#!/usr/bin/env python3
"""Compare two usability scorecards (`scorecard.json`) and print a Markdown
comment body listing the rows that changed beyond a small tolerance.

usage: scorecard_diff.py BASE.json HEAD.json [--marker TEXT]

BASE may be missing or empty (no baseline yet); every route is then new.
The first line of the output is the marker, so CI can find and update its
own comment.
"""
import json
import sys

MARKER = "<!-- noodle-scorecard-diff -->"

# (label, getter, tolerance, unit). A change smaller than the tolerance is
# noise (KLM seconds and ratios are floats), and is not reported.
METRICS = [
    ("actions", lambda r: r["effort"]["actions"], 0, ""),
    ("KLM s", lambda r: r["effort"]["klm"]["seconds"], 0.5, " s"),
    ("view switches", lambda r: r["effort"]["view_switches"], 0, ""),
    ("dialogs", lambda r: r["effort"]["dialogs_opened"], 0, ""),
    ("wait s", lambda r: r["effort"]["wait_seconds"], 0.5, " s"),
    ("hidden entry points", lambda r: r["surfaces"]["hidden_share"], 0.02, ""),
    ("undos", lambda r: r["rework"]["undos"], 0, ""),
    ("actions to hear", lambda r: (r["feedback"]["time_to_hear"] or {}).get("actions"), 0, ""),
    ("underruns", lambda r: (r["robustness"] or {}).get("underruns"), 0, ""),
]


def load(path):
    try:
        with open(path) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def safe(get, r):
    """A metric the baseline predates (an older scorecard layout) reads as
    missing, not as a crash."""
    try:
        return get(r)
    except (KeyError, TypeError):
        return None


def key(r):
    return (r["story"], r["tag"], r["name"])


def fmt(v):
    if v is None:
        return "-"
    if isinstance(v, float):
        return f"{v:.2f}".rstrip("0").rstrip(".")
    return str(v)


def changed(a, b, tol):
    if a is None or b is None:
        return a != b
    return abs(a - b) > tol


def per_item(r):
    p = r.get("per_item")
    return p["actions"] if p else None


def late_map(r):
    return {l["name"]: l for l in r.get("late_changes", [])}


def route_changes(old, new):
    out = []
    if old.get("passed") != new.get("passed"):
        out.append(("passes", "yes" if old["passed"] else "NO", "yes" if new["passed"] else "NO"))
    for label, get, tol, unit in METRICS:
        a, b = safe(get, old), safe(get, new)
        if changed(a, b, tol):
            out.append((label, fmt(a) + (unit if a is not None else ""), fmt(b) + (unit if b is not None else "")))
    a, b = safe(per_item, old), safe(per_item, new)
    if changed(a, b, 0.05):
        out.append(("actions per extra item", fmt(a), fmt(b)))
    a = safe(lambda r: r["rework"]["round_trip"], old)
    b = safe(lambda r: r["rework"]["round_trip"], new)
    if a is not None and b is not None and a != b:
        out.append(("undo round trip", "-", "ok" if b["restores_start"] and b["restores_end"] else "FAILS"))
    lo, ln = safe(late_map, old) or {}, safe(late_map, new) or {}
    for name in sorted(set(lo) | set(ln)):
        if name not in lo:
            out.append((f"late change '{name}'", "-", "new"))
        elif name not in ln:
            out.append((f"late change '{name}'", "was there", "removed"))
        else:
            for label, k, tol in (("cost", "cost", 0.02), ("KLM ratio", "klm_ratio", 0.02)):
                if changed(lo[name][k], ln[name][k], tol):
                    out.append((f"late change '{name}' {label}", fmt(lo[name][k]), fmt(ln[name][k])))
            if lo[name]["lost"] != ln[name]["lost"]:
                out.append((f"late change '{name}' loses", fmt(len(lo[name]["lost"])), fmt(len(ln[name]["lost"]))))
            if lo[name]["passed"] != ln[name]["passed"]:
                out.append((f"late change '{name}' passes", str(lo[name]["passed"]), str(ln[name]["passed"])))
    return out


def render(base, head):
    lines = [MARKER, "## Usability scorecard", ""]
    if base is None:
        lines += ["_No baseline from `main` yet, so nothing to compare against._", ""]
        base = []
    old = {key(r): r for r in base}
    new = {key(r): r for r in head}
    rows = []
    for k in sorted(new):
        label = f"{k[0]} {k[1]}: {k[2]}"
        if k not in old:
            if base:
                rows.append((label, "route", "-", "new"))
            continue
        for metric, a, b in route_changes(old[k], new[k]):
            rows.append((label, metric, a, b))
    for k in sorted(set(old) - set(new)):
        rows.append((f"{k[0]} {k[1]}: {k[2]}", "route", "was there", "removed"))
    if not rows:
        lines.append("No route changed beyond tolerance.")
    else:
        lines += ["| Route | Metric | main | this PR |", "|---|---|---|---|"]
        for label, metric, a, b in rows:
            lines.append(f"| {label} | {metric} | {a} | {b} |")
    lines += [
        "",
        "<sub>Tolerance: whole-number metrics report any change; KLM and wait seconds "
        "0.5 s; ratios 0.02. Full scorecard in the `usability-scorecard` artifact.</sub>",
    ]
    return "\n".join(lines) + "\n"


def main(argv):
    if len(argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    head = load(argv[2])
    if head is None:
        print("cannot read the head scorecard", file=sys.stderr)
        return 1
    sys.stdout.write(render(load(argv[1]), head))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
