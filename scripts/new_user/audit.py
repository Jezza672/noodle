#!/usr/bin/env python3
"""Checks that an agent stayed inside the run: it may call ctl.sh and read
screenshots, and nothing else. Reads a Claude Code stream-json transcript
(private/transcript.jsonl, or the path given) and writes private/audit.json.

    scripts/new_user/audit.py /tmp/noodle-run-s12-0 [transcript.jsonl]

A run with stray calls is "contaminated": its result says what the agent could
have found, not what a newcomer finds. Exit code 1 if contaminated. Other
runners (non-Claude) can produce a transcript in the same shape, or write their
own audit.json with {"stray": [...]}.
"""
import json
import os
import re
import shlex
import sys
from pathlib import Path


def filter_ok(words) -> bool:
    """A filter on ctl.sh's output: head or tail with a count, or grep that
    reads only its input (a pattern and options, no file named)."""
    if re.fullmatch(r"(head|tail)( -n?\+?\d+)?( -n \+?\d+)?", " ".join(words)):
        return True
    if not words or words[0] != "grep":
        return False
    with_arg = {"-A", "-B", "-C", "-m"}
    patterns_given, operands, i = False, [], 1
    while i < len(words):
        w = words[i]
        if w in with_arg:
            i += 2
            continue
        if w == "-e":
            patterns_given = True
            i += 2
            continue
        if w.startswith("-"):
            if "f" in w[1:] and not w.startswith("--") or w in ("-r", "-R") or w.startswith("--file"):
                return False  # reads a pattern file, or searches files
            i += 1
            continue
        operands.append(w)
        i += 1
    return len(operands) == (0 if patterns_given else 1)


def only_ctl(cmd: str, ctl: str) -> bool:
    """Whether a shell command line runs nothing but ctl.sh: one or more
    `bash <ctl> ...` joined by `;` or `&&`, each optionally piped to head or
    tail. Quoted text (a note) may hold anything except what the shell would
    run inside it: backticks, `$(` and `${`."""
    if "`" in cmd or "$(" in cmd or "${" in cmd:
        return False
    # Unquoted newlines separate commands, so make them visible.
    out, quote = [], None
    for ch in cmd:
        if quote:
            if ch == quote:
                quote = None
        elif ch in "'\"":
            quote = ch
        elif ch == "\n":
            ch = " ; "
        out.append(ch)
    if quote:
        return False
    lexer = shlex.shlex("".join(out), posix=True, punctuation_chars=True)
    lexer.whitespace_split = True
    try:
        tokens = list(lexer)
    except ValueError:
        return False
    segments, current = [], []
    for t in tokens:
        if t in (";", "&&", ";;"):
            segments.append(current)
            current = []
        elif t and set(t) <= set("|&;<>()") :
            if t == "|":
                current.append(t)
            else:
                return False
        else:
            current.append(t)
    segments.append(current)
    for seg in segments:
        if not seg:
            continue
        if "|" in seg:
            # ctl.sh's output through any number of filters
            parts, cur = [], []
            for w in seg:
                if w == "|":
                    parts.append(cur)
                    cur = []
                else:
                    cur.append(w)
            parts.append(cur)
            if not all(filter_ok(f) for f in parts[1:]):
                return False
            seg = parts[0]
        if len(seg) == 2 and seg[0] == "sleep" and re.fullmatch(r"\d{1,3}", seg[1]):
            continue  # waiting, as a person waits
        if len(seg) < 2 or seg[0] != "bash" or seg[1] != ctl:
            return False
    return True


def stray_calls(lines, agent: Path):
    """Calls outside the run, each marked `denied` if the tool refused it.
    Only a call that ran counts as contamination: an attempt that was denied
    taught the agent nothing."""
    ctl = str(agent / "ctl.sh")
    screens = str(agent / "screens") + "/"
    stray, by_id = [], {}
    events = []
    for line in lines:
        try:
            events.append(json.loads(line))
        except ValueError:
            continue
    denied = set()
    for ev in events:
        message = ev.get("message")
        content = message.get("content") if isinstance(message, dict) else None
        if not isinstance(content, list):
            continue
        for part in content:
            if part.get("type") == "tool_result" and "Permission to use" in json.dumps(
                part.get("content")
            ):
                denied.add(part.get("tool_use_id"))
    for ev in events:
        message = ev.get("message")
        content = message.get("content") if isinstance(message, dict) else None
        if not isinstance(content, list):
            continue
        for part in content:
            if part.get("type") != "tool_use":
                continue
            name, args = part.get("name", ""), part.get("input", {})
            if name == "Bash":
                cmd = args.get("command", "").strip()
                if not only_ctl(cmd, ctl):
                    stray.append({"tool": name, "input": cmd, "id": part.get("id")})
            elif name == "Read":
                # Resolve first: screens/../../private/result.json starts with screens/.
                path = os.path.normpath(args.get("file_path", ""))
                if not path.startswith(screens) or not path.endswith(".png"):
                    stray.append({"tool": name, "input": path, "id": part.get("id")})
            else:
                stray.append(
                    {"tool": name, "input": json.dumps(args)[:200], "id": part.get("id")}
                )
    for s in stray:
        s["denied"] = s.pop("id") in denied
    return stray


def main() -> None:
    out = Path(sys.argv[1]).resolve()
    transcript = Path(sys.argv[2]) if len(sys.argv) > 2 else out / "private/transcript.jsonl"
    if not transcript.exists():
        sys.exit(f"no transcript at {transcript}")
    stray = stray_calls(transcript.read_text().splitlines(), out / "agent")
    ran = [s for s in stray if not s["denied"]]
    (out / "private/audit.json").write_text(
        json.dumps({"contaminated": bool(ran), "stray": stray}, indent=2) + "\n"
    )
    if ran:
        print(f"audit: CONTAMINATED, {len(ran)} stray call(s) ran ({len(stray) - len(ran)} denied)")
    else:
        print(f"audit: clean ({len(stray)} attempt(s) outside the run, all denied)" if stray else "audit: clean")
    for s in ran:
        print(f"  {s['tool']}: {s['input']}")
    sys.exit(1 if ran else 0)


if __name__ == "__main__":
    main()
