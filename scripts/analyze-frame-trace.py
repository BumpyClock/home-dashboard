#!/usr/bin/env python3
"""Summarize local frame-trace JSONL using recorded CPU-side boundaries."""
import argparse
from collections import Counter, defaultdict
import json
import statistics
import sys

STAGES = {
    "render", "scene", "flush", "texture_alloc", "texture_upload",
    "page_flip_wait", "swap", "present", "render_cpu_callback",
}


def analyze(records):
    counts = Counter()
    pending = defaultdict(list)
    durations = defaultdict(list)
    photo_durations = defaultdict(list)
    render_starts = []
    incomplete = Counter()
    failures = Counter()
    animations = []
    session = None
    summary = None
    resources = []
    for record in records:
        kind = record.get("type")
        if kind == "session":
            if session is not None or record.get("schema") != 1:
                raise ValueError("duplicate or unsupported session header")
            session = record
        elif kind == "summary":
            summary = record
        elif kind == "resource":
            resources.append(record)
        elif kind == "event":
            name = record["name"]
            counts[name] += 1
            op = record.get("op_id")
            at = record["at_us"]
            fields = record.get("fields", {})
            if name == "render_begin":
                render_starts.append(at)
            elapsed = fields.get("elapsed_us")
            if name.startswith("photo.") and isinstance(elapsed, int) and not isinstance(elapsed, bool) and elapsed >= 0:
                photo_durations[(name, fields.get("kind", "unknown"))].append(elapsed)
            animation = name.startswith("photo_transition_")
            if animation:
                action = name.removeprefix("photo_transition_")
                if action not in ("start", "end", "cancel"):
                    continue
                stage = "photo_transition"
                start = action == "start"
            else:
                stage, separator, action = name.rpartition("_")
                if not separator or stage not in STAGES or action not in ("begin", "start", "end"):
                    continue
                start = action in ("begin", "start")
            if op is None:
                incomplete[f"{stage}:missing_id"] += 1
                continue
            key = (stage, op)
            if start:
                pending[key].append(record)
                continue
            if not pending[key]:
                incomplete[f"{stage}:missing_begin"] += 1
                continue
            first = pending[key].pop()
            elapsed = at - first["at_us"]
            if elapsed < 0:
                incomplete[f"{stage}:negative_duration"] += 1
                continue
            if animation:
                animations.append({"op_id": op, "surface": first.get("fields", {}).get("surface"),
                                   "start_us": first["at_us"], "end_us": at,
                                   "duration_us": elapsed, "outcome": action})
            else:
                durations[stage].append(elapsed)
                if fields.get("success") is False:
                    failures[stage] += 1
    if session is None:
        raise ValueError("missing session header")
    for (stage, op), starts in pending.items():
        if not starts:
            continue
        incomplete[f"{stage}:missing_end"] += len(starts)
        if stage == "photo_transition":
            for first in starts:
                animations.append({"op_id": op, "surface": first.get("fields", {}).get("surface"),
                                   "start_us": first["at_us"], "end_us": None,
                                   "duration_us": None, "outcome": "incomplete"})
    def distribution(values):
        values = sorted(values)
        return {"count": len(values), "total_us": sum(values),
                "min_us": values[0], "median_us": statistics.median(values),
                "p95_us": values[(95 * len(values) + 99) // 100 - 1], "max_us": values[-1]}

    stats = {stage: {**distribution(values), "failed": failures[stage]}
             for stage, values in sorted(durations.items())}
    photo_stats = [{"event": name, "kind": kind, **distribution(values)}
                   for (name, kind), values in sorted(photo_durations.items())]
    render_starts.sort()
    active_gaps = []
    for first, last in zip(render_starts, render_starts[1:]):
        overlapping = [interval for interval in animations
                       if interval["start_us"] < last and (interval["end_us"] is None or interval["end_us"] > first)]
        if overlapping:
            active_gaps.append({"start_us": first, "end_us": last, "duration_us": last - first,
                                "surfaces": sorted({i["surface"] for i in overlapping if i["surface"] is not None})})
    uncertain = (not session.get("render_hooks_compiled", False) or not render_starts
                 or summary is None or summary.get("dropped", 0) != 0
                 or summary.get("truncated_fields", 0) != 0 or bool(incomplete))
    return {"schema": 1, "session": session, "event_counts": dict(sorted(counts.items())),
            "stage_durations": stats, "photo_durations": photo_stats,
            "animation_intervals": animations, "animation_frame_gaps": active_gaps,
            "animation_frame_gaps_incomplete": uncertain,
            "incomplete_spans": dict(sorted(incomplete.items())),
            "capture_summary": summary, "summary_missing": summary is None,
            "dropped": summary.get("dropped") if summary else None,
            "truncated_fields": summary.get("truncated_fields") if summary else None,
            "resource_samples": len(resources)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trace", help="frame trace JSONL file")
    args = parser.parse_args()
    try:
        with open(args.trace, encoding="utf-8") as source:
            result = analyze(json.loads(line) for line in source if line.strip())
        json.dump(result, sys.stdout, indent=2, sort_keys=True)
        print()
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.exit(2, f"trace analysis failed: {error}\n")


if __name__ == "__main__":
    main()
