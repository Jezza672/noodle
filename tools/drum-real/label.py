#!/usr/bin/env python3
"""Interactive labelling of real drum hits for the drum classifier.

    label.py split  WORK            for songs of kind `mix`, keep only the drums (the app's HT-Demucs)
    label.py detect WORK            run the app's hit detector on every song in WORK/manifest.csv
    label.py label  WORK [--song ID] [--per-song N] [--seed N]
                                    play each hit and label it with single keystrokes
    label.py stats  WORK            how many hits are labelled, by class and purity
    label.py export WORK [OUT.csv]  write the labels as rows the dataset generator can read

WORK is a folder outside the repo (for example ~/Documents/Jezza/drum-real-work/data):

    manifest.csv          one row per song: see MANIFEST_COLUMNS
    audio/<file>          the audio, named in the manifest
    detect/<id>/          hits.json and one WAV per hit (made by `detect`)
    labels.jsonl          every labelling action, append-only (made by `label`)

No third-party packages. Playback uses afplay (macOS), else paplay, aplay or ffplay.
`detect` runs the `drum_real_detect` binary (cargo build -p noodle-eval --bin drum_real_detect,
or set DRUM_REAL_DETECT to its path), which calls the same `analyse_hits` the app uses.

Keys, in two steps for each hit (the class, then how pure it is):

    class:   k kick  s snare  c clap  h closed hat  o open hat  x crash  r ride  t tom  n other
             z not a drum hit at all (spurious: it is saved as spurious, no purity step)
             Enter takes the rule labeller's guess (shown).  Tab skips the hit (asked again later).
    purity:  Enter or 1 clean  2 double  3 truncated  4 malformed  5 mixed
    any time: Space replays  u undoes the last label  ? shows this help  q quits (all saved)

Purity names match `PURITY` in the classifier (docs/features/drum-classifier.md): clean, double
(also flams and rolls), truncated (still ringing when the cut ends), malformed (chopped start,
clipped, faded or gated), mixed (another instrument under it), spurious (not a hit).
"""
import argparse
import csv
import hashlib
import json
import os
import random
import shutil
import subprocess
import sys
import time

CLASSES = ["kick", "snare", "clap", "closed_hat", "open_hat", "crash", "ride", "tom", "other"]
CLASS_KEYS = {"k": "kick", "s": "snare", "c": "clap", "h": "closed_hat", "o": "open_hat",
              "x": "crash", "r": "ride", "t": "tom", "n": "other"}
PURITY = ["clean", "double", "truncated", "malformed", "mixed", "spurious"]
PURITY_KEYS = {"1": "clean", "2": "double", "3": "truncated", "4": "malformed", "5": "mixed"}
SPURIOUS_KEY = "z"

# What the manifest must say about every song. `kind` is multitrack (real drum tracks),
# stem (separated by the app's drum separation, drums only) or mix (only for tests).
MANIFEST_COLUMNS = ["id", "file", "title", "artist", "year", "genre", "kind", "licence",
                    "source_url", "credit"]
# Licences whose text allows use in a dataset with credit. Everything else is refused.
ALLOWED_LICENCES = {"cc0", "pd", "cc-by-2.5", "cc-by-3.0", "cc-by-4.0",
                     "cc-by-sa-2.5", "cc-by-sa-3.0", "cc-by-sa-4.0"}
KINDS = {"multitrack", "stem", "mix"}

# Columns of the dataset generator's hits.csv, so real rows sit beside synthetic ones.
CSV_HEAD = ("split,source,origin,label,onset_s,level_db,gain_db,mixed,truncated,centroid,flatness,"
            "low,mid,high,decay,pitched,pitch_key,attack_peaks")
CSV_COLUMNS = CSV_HEAD.split(",") + [f"b{i}" for i in range(24)] + ["purity", "damage"]
FEATURE_COUNT = 36


# --- manifest -----------------------------------------------------------------------------

def read_manifest(work):
    path = os.path.join(work, "manifest.csv")
    with open(path, newline="") as f:
        rows = list(csv.DictReader(f))
    for r in rows:
        missing = [c for c in MANIFEST_COLUMNS if not (r.get(c) or "").strip()]
        if missing:
            raise SystemExit(f"{path}: song {r.get('id') or '?'} is missing {', '.join(missing)}")
        if r["licence"].strip().lower() not in ALLOWED_LICENCES:
            raise SystemExit(
                f"{path}: song {r['id']} has licence {r['licence']!r}; allowed: "
                f"{', '.join(sorted(ALLOWED_LICENCES))} (no NC, no ND, nothing unclear)")
        if r["kind"] not in KINDS:
            raise SystemExit(f"{path}: song {r['id']} has kind {r['kind']!r}; use one of {sorted(KINDS)}")
    ids = [r["id"] for r in rows]
    if len(set(ids)) != len(ids):
        raise SystemExit(f"{path}: duplicate ids")
    return rows


def split_of(song_id):
    """train / val / test by a hash of the song, so all of a song's hits share a split."""
    h = int(hashlib.sha256(song_id.encode()).hexdigest()[:8], 16) % 100
    return "train" if h < 70 else "val" if h < 85 else "test"


# --- detection ----------------------------------------------------------------------------

def tool_path(name, env):
    p = os.environ.get(env)
    if p:
        return p
    root = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
    for profile in ("release", "debug"):
        c = os.path.join(root, "target", profile, name)
        if os.path.exists(c):
            return c
    return shutil.which(name)


def drums_file(work, r):
    """The audio to detect in: the file itself, or for a `mix` its drums-only split."""
    if r["kind"] == "mix":
        return os.path.join(work, "stems", f"{r['id']}-drums.wav")
    return os.path.join(work, "audio", r["file"])


def split_mixes(work):
    exe = tool_path("drum_real_split", "DRUM_REAL_SPLIT")
    if not exe:
        raise SystemExit("no drum_real_split binary: cargo build -p noodle-eval --bin drum_real_split")
    os.makedirs(os.path.join(work, "stems"), exist_ok=True)
    for r in read_manifest(work):
        out = drums_file(work, r)
        if r["kind"] != "mix" or os.path.exists(out):
            continue
        print(f"{r['id']}: separating drums ...", flush=True)
        res = subprocess.run([exe, os.path.join(work, "audio", r["file"]), out + ".part"], capture_output=True, text=True)
        if res.returncode != 0:
            print(f"  failed: {res.stderr.strip()}")
        else:
            os.replace(out + ".part", out)


def detect(work):
    exe = tool_path("drum_real_detect", "DRUM_REAL_DETECT")
    if not exe:
        raise SystemExit("no drum_real_detect binary: cargo build -p noodle-eval --bin drum_real_detect")
    for r in read_manifest(work):
        out = os.path.join(work, "detect", r["id"])
        if os.path.exists(os.path.join(out, "hits.json")):
            print(f"{r['id']}: already detected")
            continue
        audio = drums_file(work, r)
        if not os.path.exists(audio):
            print(f"{r['id']}: no drums file yet (run split)")
            continue
        print(f"{r['id']}: detecting ...", flush=True)
        res = subprocess.run([exe, audio, out], capture_output=True, text=True)
        if res.returncode != 0:
            print(f"  failed: {res.stderr.strip()}")
        else:
            print(f"  {res.stdout.strip()}")


def load_hits(work, song_id):
    p = os.path.join(work, "detect", song_id, "hits.json")
    if not os.path.exists(p):
        return None
    with open(p) as f:
        return json.load(f)


# --- labels (append-only log) -------------------------------------------------------------

class Labels:
    """The labelling state, replayed from `labels.jsonl`.

    Each line is {"song", "id", "class", "purity", "t"} for a label (class is null when
    spurious), {"song", "id", "skip": true} for a skip, or {"undo": true}, which removes the
    newest label or skip still standing. The log is only ever appended to.
    """

    def __init__(self, path):
        self.path = path
        self.stack = []  # events still standing, oldest first
        if os.path.exists(path):
            with open(path) as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    ev = json.loads(line)
                    if ev.get("undo"):
                        if self.stack:
                            self.stack.pop()
                    else:
                        self.stack.append(ev)

    def _append(self, ev):
        with open(self.path, "a") as f:
            f.write(json.dumps(ev) + "\n")
            f.flush()
            os.fsync(f.fileno())

    def label(self, song, hit_id, cls, purity):
        ev = {"song": song, "id": hit_id, "class": cls, "purity": purity, "t": int(time.time())}
        self._append(ev)
        self.stack.append(ev)

    def skip(self, song, hit_id):
        ev = {"song": song, "id": hit_id, "skip": True}
        self._append(ev)
        self.stack.append(ev)

    def undo(self):
        """Removes and returns the newest standing event, or None."""
        if not self.stack:
            return None
        self._append({"undo": True})
        return self.stack.pop()

    def labelled(self):
        """{(song, id): event} of the hits with a label standing (the newest per hit)."""
        out = {}
        for ev in self.stack:
            if not ev.get("skip"):
                out[(ev["song"], ev["id"])] = ev
        return out

    def skipped(self):
        done = self.labelled()
        return {(e["song"], e["id"]) for e in self.stack if e.get("skip")} - set(done)


def queue(work, songs, labels, per_song, seed, only=None):
    """The hits still to label: a seeded random sample of `per_song` hits of each song
    (so the order is the same every run and a resumed session carries on), songs in
    manifest order, hits in time order within the sample. Skipped hits come last."""
    done = labels.labelled()
    skipped = labels.skipped()
    pending, later = [], []
    for s in songs:
        if only and s["id"] != only:
            continue
        data = load_hits(work, s["id"])
        if not data or not data["hits"]:
            continue
        hits = sorted(data["hits"], key=lambda h: h["id"])
        rng = random.Random(f"{seed}:{s['id']}")
        sample = rng.sample(hits, min(per_song, len(hits)))
        sample.sort(key=lambda h: h["onset_s"])
        for h in sample:
            key = (s["id"], h["id"])
            if key in done:
                continue
            (later if key in skipped else pending).append((s, data, h))
    return pending + later


# --- playback and keys --------------------------------------------------------------------

class Player:
    def __init__(self):
        self.proc = None
        self.cmd = next((c for c in (["afplay"], ["paplay"], ["aplay", "-q"], ["ffplay", "-nodisp", "-autoexit", "-loglevel", "quiet"])
                         if shutil.which(c[0])), None)

    def play(self, path):
        self.stop()
        if self.cmd:
            self.proc = subprocess.Popen(self.cmd + [path], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def stop(self):
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()
        self.proc = None


def read_key():
    """One keystroke, without Enter. Enter comes back as "\\n", Tab as "\\t"."""
    import termios
    import tty
    fd = sys.stdin.fileno()
    old = termios.tcgetattr(fd)
    try:
        tty.setraw(fd)
        ch = sys.stdin.read(1)
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old)
    if ch == "\x03":
        raise KeyboardInterrupt
    if ch == "\r":
        return "\n"
    return ch


HELP = """
 class:   k kick  s snare  c clap  h closed hat  o open hat  x crash  r ride  t tom  n other
          z not a hit (spurious)   Enter take the guess   Tab skip
 purity:  Enter/1 clean  2 double  3 truncated  4 malformed  5 mixed
 always:  Space replay   u undo   ? help   q quit (everything is saved)
"""


def guess_class(hit):
    """The rule labeller's guess as one of CLASSES (`Closed hat` -> closed_hat; either tom -> tom)."""
    g = hit["guess"].lower().replace(" ", "_")
    if g.endswith("_tom"):
        g = "tom"
    return g if g in CLASSES else "other"


def describe(song, hit, n, total, done_today):
    dur = hit["end_s"] - hit["start_s"]
    flags = ", ".join(f for f in ("truncated" if hit["truncated"] else "", "mixed" if hit["mixed"] else "") if f)
    return (f"[{n}/{total}] {song['title']} ({song['year']}, {song['genre']}) @ {hit['onset_s']:.2f}s  "
            f"{dur * 1000:.0f} ms  {hit['level_db']:.0f} dB"
            f"{'  detector: ' + flags if flags else ''}  guess: {guess_class(hit)}")


def session(work, songs, labels, per_song, seed, only, key=read_key, out=sys.stdout, player=None):
    player = player or Player()
    if not player.cmd:
        print("no audio player found (afplay, paplay, aplay, ffplay)", file=out)
    items = queue(work, songs, labels, per_song, seed, only)
    total_new = len(items)
    if not items:
        print("nothing left to label", file=out)
        return 0
    print(HELP, file=out)
    i = 0
    count = 0
    while i < len(items):
        song, data, hit = items[i]
        wav = os.path.join(work, "detect", song["id"], hit["wav"])
        print("\r\n" + describe(song, hit, i + 1, len(items), count), file=out)
        player.play(wav)
        cls = None
        while True:  # class step
            k = key()
            if k == " ":
                player.play(wav)
            elif k in CLASS_KEYS:
                cls = CLASS_KEYS[k]
                break
            elif k == SPURIOUS_KEY:
                cls = "spurious"
                break
            elif k == "\n":
                cls = guess_class(hit)
                break
            elif k == "\t":
                labels.skip(song["id"], hit["id"])
                cls = "skip"
                break
            elif k == "u":
                ev = labels.undo()
                if ev is None:
                    print("nothing to undo", file=out)
                    continue
                items = queue(work, songs, labels, per_song, seed, only)
                count = max(0, count - 1)
                i = 0
                print(f"undid {ev.get('class') or 'skip'} {ev.get('purity') or ''}", file=out)
                cls = "undo"
                break
            elif k == "?":
                print(HELP, file=out)
            elif k in ("q", "\x1b"):
                player.stop()
                print(f"saved. {count} labelled this session.", file=out)
                return count
        if cls == "undo":
            continue
        if cls == "skip":
            print("  skipped", file=out)
            i += 1
            continue
        if cls == "spurious":
            labels.label(song["id"], hit["id"], None, "spurious")
            print("  not a hit", file=out)
            count += 1
            i += 1
            continue
        print(f"  {cls}: purity? (Enter = clean)", file=out)
        purity = None
        while purity is None:
            k = key()
            if k == " ":
                player.play(wav)
            elif k == "\n":
                purity = "clean"
            elif k in PURITY_KEYS:
                purity = PURITY_KEYS[k]
            elif k == "\t":
                break
            elif k in ("q", "\x1b"):
                player.stop()
                print(f"saved. {count} labelled this session.", file=out)
                return count
            elif k == "?":
                print(HELP, file=out)
        if purity is None:  # Tab at the purity step: back to the class step
            continue
        labels.label(song["id"], hit["id"], cls, purity)
        print(f"  {cls}, {purity}", file=out)
        count += 1
        i += 1
    if any(labels.skipped()):  # skipped hits come round again
        items = queue(work, songs, labels, per_song, seed, only)
        if items:
            return count + session(work, songs, labels, per_song, seed, only, key, out, player)
    player.stop()
    print(f"all done: {count} labelled this session.", file=out)
    return count


# --- stats and export ---------------------------------------------------------------------

def stats(work):
    labels = Labels(os.path.join(work, "labels.jsonl"))
    done = labels.labelled()
    if not done:
        print("no labels yet")
        return
    by_class, by_purity, by_song = {}, {}, {}
    for (song, _), ev in done.items():
        by_class[ev["class"] or "-"] = by_class.get(ev["class"] or "-", 0) + 1
        by_purity[ev["purity"]] = by_purity.get(ev["purity"], 0) + 1
        by_song[song] = by_song.get(song, 0) + 1
    print(f"{len(done)} labelled hits in {len(by_song)} songs")
    print("class: ", ", ".join(f"{k} {v}" for k, v in sorted(by_class.items())))
    print("purity:", ", ".join(f"{k} {v}" for k, v in sorted(by_purity.items())))


def export_rows(work):
    """Rows for the dataset generator, one per labelled hit, in hits.csv's columns."""
    songs = {s["id"]: s for s in read_manifest(work)}
    labels = Labels(os.path.join(work, "labels.jsonl"))
    rows = []
    for (song_id, hit_id), ev in sorted(labels.labelled().items()):
        song = songs.get(song_id)
        data = load_hits(work, song_id)
        if song is None or data is None:
            continue
        hit = next((h for h in data["hits"] if h["id"] == hit_id), None)
        if hit is None:
            continue
        f = hit["feat"]
        v = hit["features"]
        # The vector the classifier takes must agree with the named fields (drum_real_detect
        # tests feature_vector's order in Rust).
        want = f["bands"] + [f["centroid"], f["flatness"], f["low"], f["mid"], f["high"], f["decay"],
                             f["pitched"], f["pitch_key"] or 0.0, float(f["pitch_key"] is not None),
                             float(f["attack_peaks"]), float(hit["mixed"]), float(hit["truncated"])]
        if len(v) != FEATURE_COUNT or any(abs(x - y) > 1e-4 for x, y in zip(v, want)):
            raise SystemExit(f"{song_id} hit {hit_id}: features do not match their names; re-run detect")
        bands = f["bands"]
        centroid, flatness, low, mid, high = f["centroid"], f["flatness"], f["low"], f["mid"], f["high"]
        decay, pitched, pitch_key, peaks = f["decay"], f["pitched"], f["pitch_key"], f["attack_peaks"]
        row = {
            "split": split_of(song_id),
            "source": f"real:{song_id}",
            "origin": song["licence"],
            "label": ev["class"] or "spurious",
            "onset_s": f"{hit['onset_s']:.4f}",
            "level_db": f"{hit['level_db']:.2f}",
            "gain_db": "0.0",
            # The detector's own flags, as in the feature vector: never the human purity label.
            "mixed": int(hit["mixed"]),
            "truncated": int(hit["truncated"]),
            "centroid": f"{centroid:.2f}",
            "flatness": f"{flatness:.5f}",
            "low": f"{low:.5f}", "mid": f"{mid:.5f}", "high": f"{high:.5f}",
            "decay": f"{decay:.4f}", "pitched": f"{pitched:.4f}",
            "pitch_key": f"{pitch_key:.3f}" if pitch_key is not None else "",
            "attack_peaks": int(peaks),
            "purity": ev["purity"],
            "damage": "real",
        }
        for b in range(24):
            row[f"b{b}"] = f"{bands[b]:.3f}"
        rows.append(row)
    return rows


def export(work, out):
    rows = export_rows(work)
    with open(out, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=CSV_COLUMNS, lineterminator="\n")
        w.writeheader()
        w.writerows(rows)
    print(f"{len(rows)} rows to {out}")


# --- main ---------------------------------------------------------------------------------

def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("split", "detect", "label", "stats", "export"):
        p = sub.add_parser(name)
        p.add_argument("work")
        if name == "label":
            p.add_argument("--song")
            p.add_argument("--per-song", type=int, default=150, help="hits to label in each song (default 150)")
            p.add_argument("--seed", type=int, default=1)
        if name == "export":
            p.add_argument("out", nargs="?")
    a = ap.parse_args(argv)
    work = os.path.expanduser(a.work)
    if a.cmd == "split":
        split_mixes(work)
    elif a.cmd == "detect":
        detect(work)
    elif a.cmd == "label":
        songs = read_manifest(work)
        session(work, songs, Labels(os.path.join(work, "labels.jsonl")), a.per_song, a.seed, a.song)
    elif a.cmd == "stats":
        stats(work)
    elif a.cmd == "export":
        export(work, a.out or os.path.join(work, "real_hits.csv"))


if __name__ == "__main__":
    main()
