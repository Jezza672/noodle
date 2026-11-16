"""Trains the drum hit classifier (issue #499) on the dataset that
`noodle-eval drum-dataset build` writes, and exports it to ONNX.

    python train.py DATASET_DIR OUT_DIR [--seed N] [--epochs N]

The model is a small MLP over the detector's feature vector (see FEATURES).
Normalisation lives inside the ONNX graph, so the app feeds raw features.
Outputs: classifier.onnx, report.md (test-split metrics) and model-card.md.
"""
import argparse, csv, json, math, os, sys
import numpy as np
import torch
from torch import nn

CLASSES = ["kick", "snare", "clap", "closed_hat", "open_hat", "crash", "ride", "tom", "other"]
# The input vector, in order. The Rust side builds exactly this.
FEATURES = (
    [f"b{i}" for i in range(24)]
    + ["centroid_hz", "flatness", "low", "mid", "high", "decay_s", "pitched", "pitch_key", "has_pitch", "attack_peaks", "mixed", "truncated"]
)
# How pure a detection is as a kit sample, in the order of the `purity` output.
# Only `clean` may enter a bin of single hits (docs/features/drum-classifier.md).
PURITY = ["clean", "double", "truncated", "malformed", "mixed", "spurious"]
# The false-accept rate (not-clean hits let into a clean bin) the gate is tuned for.
TARGET_FAR = 0.01
QUIET_DB = -45.0
# A loop hit labelled from a placed hit mixed this far down is mostly someone
# else's tail, so it is neither trained on nor scored.
MIN_GAIN_DB = -35.0


def load(path, extra=()):
    """Rows of `path`/hits.csv, and of each file in `extra` (hand-labelled real
    hits exported by tools/drum-real/label.py, same columns, source `real`)."""
    rows = {"train": [], "val": [], "test": []}
    files = [os.path.join(path, "hits.csv")] + list(extra)
    for name in files:
      with open(name) as f:
        for r in csv.DictReader(f):
            if float(r["level_db"]) < QUIET_DB or float(r["gain_db"]) < MIN_GAIN_DB:
                continue
            ty = CLASSES.index(r["label"]) if r["label"] in CLASSES else -1
            pk = r["pitch_key"]
            x = [float(r[f"b{i}"]) for i in range(24)] + [
                float(r["centroid"]), float(r["flatness"]), float(r["low"]), float(r["mid"]),
                float(r["high"]), float(r["decay"]), float(r["pitched"]),
                float(pk) if pk else 0.0, 1.0 if pk else 0.0, float(r["attack_peaks"]),
                float(r["mixed"]), float(r["truncated"]),
            ]
            rows[r["split"]].append((x, ty, r["source"], r["mixed"] == "1" or r["truncated"] == "1", float(pk) if pk else None, PURITY.index(r["purity"]), r["damage"]))
    return rows


def arrays(rs):
    return (
        torch.tensor([r[0] for r in rs], dtype=torch.float32),
        torch.tensor([r[1] for r in rs]),
    )


def purity_of(rs):
    return torch.tensor([r[5] for r in rs])


class Net(nn.Module):
    def __init__(self, mean, std, hidden=256):
        super().__init__()
        self.register_buffer("mean", mean)
        self.register_buffer("std", std)
        self.body = nn.Sequential(
            nn.Linear(len(FEATURES), hidden), nn.ReLU(), nn.Dropout(0.3),
            nn.Linear(hidden, hidden), nn.ReLU(), nn.Dropout(0.3),
        )
        self.kind = nn.Linear(hidden, len(CLASSES))
        self.purity = nn.Linear(hidden, len(PURITY))

    def prep(self, x):
        x = x.clone()
        x[:, 24] = torch.log2(x[:, 24].clamp(min=20.0))  # centroid
        x[:, 29] = torch.log(x[:, 29].clamp(min=1e-3))  # decay
        return (x - self.mean) / self.std

    def forward(self, x):
        h = self.body(self.prep(x))
        return self.kind(h), self.purity(h)


class Exported(nn.Module):
    """Raw features in; class probabilities and the top probability out."""
    def __init__(self, net):
        super().__init__()
        self.net = net

    def forward(self, features):
        kind, purity = self.net(features)
        p = torch.softmax(kind, dim=-1)
        return p, p.max(dim=-1).values, torch.softmax(purity, dim=-1)


def predict(net, x):
    net.eval()
    with torch.no_grad():
        return torch.softmax(net(x)[0], dim=-1)


def predict_purity(net, x):
    net.eval()
    with torch.no_grad():
        return torch.softmax(net(x)[1], dim=-1)


def tune_gate(p, y):
    """The lowest gate on `clean` probability with false-accept at most TARGET_FAR
    (of real not-clean hits; spurious detections are not counted), and the share
    of clean hits that passes it."""
    pc = p[:, 0]
    bad = pc[(y != 0) & (y != PURITY.index("spurious"))].sort().values
    clean = pc[y == 0]
    if len(bad) == 0 or len(clean) == 0:
        return 0.9, 0.0
    allowed = int(TARGET_FAR * len(bad))
    gate = max(0.5, float(bad[-allowed - 1]) + 1e-6)
    return gate, float((clean >= gate).float().mean())


def report(net, rows, rule_rows=None):
    out = []
    for title, pick in [
        ("One-shots, alone", lambda r: r[2] == "oneshot"),
        ("Loops, all hits", lambda r: r[2] == "loop"),
        ("Loops, clean hits only", lambda r: r[2] == "loop" and not r[3]),
    ]:
        rs = [r for r in rows if pick(r) and r[1] >= 0 and r[5] != PURITY.index("spurious")]
        if not rs:
            continue
        x, y = arrays(rs)
        p = predict(net, x)
        conf, pred = p.max(dim=1)
        n = len(rs)
        for thr in (0.0, 0.5, 0.7):
            known = conf >= thr
            # Below the threshold the app says Unknown, which is right for `other`.
            right = ((pred == y) & known) | ((y == CLASSES.index("other")) & (~known | (pred == y)))
            wrong_conf = known & (pred != y) & (pred != CLASSES.index("other"))
            out.append(f"{title}, threshold {thr}: {n} hits, {100*right.float().mean():.1f}% right, {100*wrong_conf.float().mean():.1f}% confidently wrong")
        conf_thr = 0.5
        known = conf >= conf_thr
        out.append("")
        out.append("| truth \\ guess | " + " | ".join(CLASSES) + " | Unknown | n | recall |")
        out.append("|---|" + "---|" * (len(CLASSES) + 3))
        for ci, c in enumerate(CLASSES):
            m = y == ci
            if m.sum() == 0:
                continue
            cells = []
            for pj in range(len(CLASSES)):
                cells.append(int(((pred == pj) & known & m).sum()))
            unk = int((~known & m).sum())
            right = cells[ci] if c != "other" else unk + cells[ci]
            out.append(f"| {c} | " + " | ".join(map(str, cells)) + f" | {unk} | {int(m.sum())} | {100*right/int(m.sum()):.0f}% |")
        out.append("")
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("data")
    ap.add_argument("out")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--epochs", type=int, default=60)
    ap.add_argument("--real", action="append", default=[], help="a hand-labelled hits.csv to add (repeatable)")
    a = ap.parse_args()
    torch.manual_seed(a.seed)
    np.random.seed(a.seed)
    rows = load(a.data, a.real)
    xtr, ytr = arrays(rows["train"])
    xva, yva = arrays(rows["val"])
    ptr, pva = purity_of(rows["train"]), purity_of(rows["val"])
    probe = xtr.clone()
    probe[:, 24] = torch.log2(probe[:, 24].clamp(min=20.0))
    probe[:, 29] = torch.log(probe[:, 29].clamp(min=1e-3))
    mean, std = probe.mean(0), probe.std(0).clamp(min=1e-3)
    for i in (32, 34, 35):  # the flags go in as they are
        mean[i], std[i] = 0.0, 1.0
    net = Net(mean, std)
    kinds = ytr[ytr >= 0]
    counts = torch.bincount(kinds, minlength=len(CLASSES)).float().clamp(min=1)
    weight = (counts.sum() / counts / len(CLASSES)).sqrt()
    loss_fn = nn.CrossEntropyLoss(weight=weight)
    pcounts = torch.bincount(ptr, minlength=len(PURITY)).float().clamp(min=1)
    purity_loss = nn.CrossEntropyLoss(weight=(pcounts.sum() / pcounts / len(PURITY)).sqrt())
    opt = torch.optim.AdamW(net.parameters(), lr=3e-3, weight_decay=1e-3)
    sched = torch.optim.lr_scheduler.CosineAnnealingLR(opt, a.epochs)
    best, best_state = 0.0, None
    for epoch in range(a.epochs):
        net.train()
        perm = torch.randperm(len(xtr))
        for i in range(0, len(perm), 256):
            idx = perm[i:i + 256]
            xb = xtr[idx]
            # Small noise on the spectrum, so it does not memorise detector jitter.
            xb = xb.clone()
            xb[:, :24] += torch.randn(len(idx), 24) * 1.0
            opt.zero_grad()
            kind_out, purity_out = net(xb)
            has_type = ytr[idx] >= 0
            loss = purity_loss(purity_out, ptr[idx])
            if has_type.any():
                loss = loss + loss_fn(kind_out[has_type], ytr[idx][has_type])
            loss.backward()
            opt.step()
        sched.step()
        # The best epoch is the one that keeps the most clean hits at the target
        # false-accept rate on validation: that is what the bins use.
        _, acc = tune_gate(predict_purity(net, xva), pva)
        if acc > best:
            best, best_state = acc, {k: v.clone() for k, v in net.state_dict().items()}
    net.load_state_dict(best_state)
    gate, kept = tune_gate(predict_purity(net, xva), pva)
    print(f"clean gate {gate:.4f}: {100*kept:.1f}% of clean validation hits kept at false-accept <= {100*TARGET_FAR:.1f}%")
    os.makedirs(a.out, exist_ok=True)
    net.eval()
    ex = Exported(net).eval()
    torch.onnx.export(
        ex, (torch.zeros(1, len(FEATURES)),), os.path.join(a.out, "classifier.onnx"),
        input_names=["features"], output_names=["probs", "confidence", "purity"],
        dynamic_axes={"features": {0: "n"}, "probs": {0: "n"}, "confidence": {0: "n"}, "purity": {0: "n"}},
        opset_version=17, dynamo=False,
    )
    import onnxruntime as ort
    sess = ort.InferenceSession(os.path.join(a.out, "classifier.onnx"))
    xt = arrays(rows["test"])[0][:200]
    got = sess.run(None, {"features": xt.numpy()})
    assert np.allclose(got[0], predict(net, xt).numpy(), atol=1e-4), "ONNX output differs from PyTorch"
    assert np.allclose(got[2], predict_purity(net, xt).numpy(), atol=1e-4), "ONNX purity differs from PyTorch"
    rep = f"# Learned classifier, test split\n\nClean gate tuned on validation: {gate:.4f} ({100*kept:.1f}% of clean hits kept at false-accept <= {100*TARGET_FAR:.1f}%), {a.epochs} epochs, seed {a.seed}.\nThe purity gate is scored in Rust: `noodle-eval drum-dataset baseline --model classifier.onnx --gate {gate:.4f}`.\n\n" + report(net, rows["test"])
    open(os.path.join(a.out, "report.md"), "w").write(rep)
    json.dump({"classes": CLASSES, "purity": PURITY, "clean_gate": gate, "features": FEATURES, "quiet_db": QUIET_DB}, open(os.path.join(a.out, "io.json"), "w"), indent=1)
    print(rep)


if __name__ == "__main__":
    main()
