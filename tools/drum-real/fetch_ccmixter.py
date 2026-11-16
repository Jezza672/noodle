#!/usr/bin/env python3
"""Downloads openly licensed audio from ccMixter into a drum-real WORK folder and records
source, licence and credit for each file in WORK/manifest.csv (see label.py).

    fetch_ccmixter.py WORK --tags funk,drums [--kind mix|multitrack|stem] [--era 1970s] [--genre funk]
                      [--count 3] [--max-mb 20]

Only CC0 (`zero`) and CC BY / CC BY-SA uploads are kept: the licence is read from each
upload's `license_url`, not from the query, and anything else (NC, ND, sampling, unclear) is
skipped. Existing manifest rows are never changed; an upload already in the manifest is skipped.
`--kind mix` means a finished song (split it with the app's drum separation before labelling
and record the result as kind `stem`); `--kind stem` means the upload is already drums only
(for example a drum loop or a drum stem).
"""
import argparse, csv, json, os, re, sys, urllib.parse, urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import label

API = "https://ccmixter.org/api/query"
UA = "noodle-drum-real/1 (dataset builder; open-licence audio only)"
LICENCE_URL = re.compile(r"^https?://creativecommons\.org/(licenses/(by|by-sa)/(\d\.\d)|publicdomain/zero/1\.0)/?", re.I)


def licence_of(url):
    m = LICENCE_URL.match(url or "")
    if not m:
        return None
    if m.group(2):
        return f"cc-{m.group(2).lower()}-{m.group(3)}"
    return "cc0"


def get(url):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read()


def query(tags, lic, pages=4, page_size=8):
    """Uploads with all of `tags`, best ranked first. Pages are small: the server breaks
    (a response header over 64 KB) when a page is large."""
    out = []
    for p in range(pages):
        q = {"f": "json", "limit": page_size, "offset": p * page_size, "lic": lic, "tags": tags, "sort": "rank"}
        got = json.loads(get(API + "?" + urllib.parse.urlencode(q)))
        out += got
        if len(got) < page_size:
            break
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("work")
    ap.add_argument("--tags", required=True, help="ccMixter tags, all required (comma separated)")
    ap.add_argument("--kind", default="mix", choices=sorted(label.KINDS))
    ap.add_argument("--era", default="unknown", help="era the sound is typical of, e.g. 1970s")
    ap.add_argument("--genre", help="genre to record (default: the first tag)")
    ap.add_argument("--count", type=int, default=3)
    ap.add_argument("--max-mb", type=float, default=20)
    ap.add_argument("--min-seconds", type=float, default=30, help="skip shorter uploads (drum loops: pass 0)")
    a = ap.parse_args(argv)
    work = os.path.expanduser(a.work)
    os.makedirs(os.path.join(work, "audio"), exist_ok=True)
    mpath = os.path.join(work, "manifest.csv")
    rows = []
    if os.path.exists(mpath):
        with open(mpath, newline="") as f:
            rows = list(csv.DictReader(f))
    known = {r["source_url"] for r in rows}
    got = 0
    for lic in ("zero", "by"):
        for u in query(a.tags, lic):
            if got >= a.count:
                break
            lic_id = licence_of(u.get("license_url"))
            page = u["file_page_url"]
            if not lic_id or page in known:
                continue
            files = [f for f in u["files"] if f["file_format_info"].get("default-ext") in ("mp3", "wav", "flac", "ogg")]
            files = [f for f in files if f.get("file_rawsize", 0) <= a.max_mb * 1e6 and not f.get("file_is_remote")]
            if not files:
                continue
            f = max(files, key=lambda f: f["file_rawsize"])
            secs = f["file_format_info"].get("ps", "0:0").split(":")
            try:
                dur = int(secs[0]) * 60 + int(secs[1])
            except ValueError:
                dur = 0
            if dur < a.min_seconds:
                continue
            sid = f"ccm{u['upload_id']}"
            name = f"{sid}.{f['file_format_info']['default-ext']}"
            dest = os.path.join(work, "audio", name)
            print(f"{sid}: {u['upload_name']} by {u['user_real_name']} ({lic_id}, {dur}s) ...", flush=True)
            try:
                data = get(f["download_url"])
            except Exception as e:
                print(f"  download failed: {e}")
                continue
            with open(dest, "wb") as out:
                out.write(data)
            title = u["upload_name"].replace("\n", " ")
            artist = u["user_real_name"] or u["user_name"]
            rows.append({
                "id": sid, "file": name, "title": title, "artist": artist, "year": a.era,
                "genre": a.genre or a.tags.split(",")[0], "kind": a.kind, "licence": lic_id,
                "source_url": page,
                "credit": f'"{title}" by {artist}, ccMixter ({u["license_name"]}, {u["license_url"]})',
            })
            known.add(page)
            got += 1
    with open(mpath, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=label.MANIFEST_COLUMNS, lineterminator="\n")
        w.writeheader()
        w.writerows(rows)
    print(f"{got} added; manifest has {len(rows)} songs")


if __name__ == "__main__":
    main()
