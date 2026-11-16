import csv, io, json, os, sys, tempfile, unittest

sys.path.insert(0, os.path.dirname(__file__))
import label


class FakePlayer:
    cmd = ["fake"]
    def play(self, p): pass
    def stop(self): pass


def make_work(licence="cc-by-4.0", n=4):
    work = tempfile.mkdtemp()
    os.makedirs(os.path.join(work, "detect", "s1"))
    with open(os.path.join(work, "manifest.csv"), "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(label.MANIFEST_COLUMNS)
        w.writerow(["s1", "s1.wav", "Song", "Artist", "1975", "funk", "multitrack", licence, "http://x", "Artist, CC BY"])
    hits = [{"id": i, "wav": f"{i:04}.wav", "onset_s": i * 0.5, "start_s": i * 0.5, "end_s": i * 0.5 + 0.2,
             "truncated": False, "mixed": i == 1, "level_db": -10.0, "guess": "Kick", "guess_confidence": 0.5,
             "feat": {"bands": [float(-j) for j in range(24)], "centroid": 1000.0, "flatness": 0.2, "low": 0.5,
                      "mid": 0.3, "high": 0.2, "decay": 0.1, "pitched": 1.0, "pitch_key": 36.0, "attack_peaks": 1},
             "features": [float(-j) for j in range(24)] + [1000.0, 0.2, 0.5, 0.3, 0.2, 0.1, 1.0, 36.0, 1.0, 1.0, 1.0 if i == 1 else 0.0, 0.0],
             "pitch_key": 36.0} for i in range(n)]
    with open(os.path.join(work, "detect", "s1", "hits.json"), "w") as f:
        json.dump({"audio": "x", "sample_rate": 44100, "duration_s": 10, "hits": hits}, f)
    return work


def keys(seq):
    it = iter(seq)
    return lambda: next(it)


def run(work, seq, per_song=150):
    labels = label.Labels(os.path.join(work, "labels.jsonl"))
    label.session(work, label.read_manifest(work), labels, per_song, 1, None, key=keys(seq), out=io.StringIO(), player=FakePlayer())
    return label.Labels(os.path.join(work, "labels.jsonl"))


class Tests(unittest.TestCase):
    def test_labels_classes_and_purity(self):
        w = make_work()
        # kick clean(Enter), snare double, spurious, open hat (Enter = clean), then quit
        l = run(w, ["k", "\n", "s", "2", "z", "o", "\n", "q"])
        got = {k[1]: (e["class"], e["purity"]) for k, e in l.labelled().items()}
        self.assertEqual(got, {0: ("kick", "clean"), 1: ("snare", "double"), 2: (None, "spurious"), 3: ("open_hat", "clean")})

    def test_undo_and_resume(self):
        w = make_work()
        l = run(w, ["k", "1", "s", "1", "u", "t", "4", "q"])  # undo snare, relabel as tom malformed
        got = {k[1]: (e["class"], e["purity"]) for k, e in l.labelled().items()}
        self.assertEqual(got, {0: ("kick", "clean"), 1: ("tom", "malformed")})
        l = run(w, ["q"])  # resumes at hit 2
        l = run(w, ["u", "q"])  # undo survives a restart
        self.assertEqual(len(l.labelled()), 1)

    def test_skip_comes_back_last(self):
        w = make_work(n=3)
        l = run(w, ["\t", "k", "1", "s", "1", "k", "5"])  # skip hit 0, label 1 and 2, then hit 0 again
        got = {k[1]: (e["class"], e["purity"]) for k, e in l.labelled().items()}
        self.assertEqual(got, {0: ("kick", "mixed"), 1: ("kick", "clean"), 2: ("snare", "clean")})

    def test_enter_takes_guess_and_per_song_cap(self):
        w = make_work(n=10)
        l = run(w, ["\n", "\n", "\n", "\n"], per_song=2)
        self.assertEqual(len(l.labelled()), 2)

    def test_export_matches_dataset_columns(self):
        w = make_work()
        run(w, ["k", "5", "z", "q"])
        rows = label.export_rows(w)
        self.assertEqual(len(rows), 2)
        for r in rows:
            self.assertEqual(set(r), set(label.CSV_COLUMNS))
        by = {r["label"]: r for r in rows}
        # human label "mixed" on hit 0, whose detector flags are both off: the columns keep the flags
        self.assertEqual(by["kick"]["purity"], "mixed")
        self.assertEqual((by["kick"]["mixed"], by["kick"]["truncated"]), (0, 0))
        self.assertEqual(by["kick"]["pitch_key"], "36.000")
        self.assertEqual(by["spurious"]["purity"], "spurious")
        self.assertEqual(by["kick"]["source"], "real:s1")
        self.assertIn(by["kick"]["split"], ("train", "val", "test"))

    def test_guess_names(self):
        for g, want in (("Kick", "kick"), ("Closed hat", "closed_hat"), ("Low tom", "tom"), ("Unknown", "other")):
            self.assertEqual(label.guess_class({"guess": g}), want)

    def test_split_is_stable_per_song(self):
        self.assertEqual(label.split_of("abc"), label.split_of("abc"))
        counts = {s: 0 for s in ("train", "val", "test")}
        for i in range(1000):
            counts[label.split_of(f"song{i}")] += 1
        self.assertTrue(600 < counts["train"] < 800 and counts["test"] > 100)

    def test_closed_licences_are_flagged_not_refused(self):
        for lic, open_ in (("cc-by-4.0", True), ("all-rights-reserved", False), ("cc-by-nc-4.0", False)):
            rows = label.read_manifest(make_work(licence=lic))
            self.assertEqual(label.redistributable(rows[0]), open_)

    def test_purity_names_match_classifier(self):
        # drum_data.rs Purity::ALL (docs/features/drum-classifier.md)
        self.assertEqual(label.PURITY, ["clean", "double", "truncated", "malformed", "mixed", "spurious"])


if __name__ == "__main__":
    unittest.main()
