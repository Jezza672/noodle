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
QUIET_DB = -45.0
# A loop hit labelled from a placed hit mixed this far down is mostly someone
# else's tail, so it is neither trained on nor scored.
MIN_GAIN_DB = -35.0


def load(path):
    rows = {"train": [], "val": [], "test": []}
    with open(os.path.join(path, "hits.csv")) as f:
        for r in csv.DictReader(f):
            if r["label"] not in CLASSES or float(r["level_db"]) < QUIET_DB or float(r["gain_db"]) < MIN_GAIN_DB:
                continue
            pk = r["pitch_key"]
            x = [float(r[f"b{i}"]) for i in range(24)] + [
                float(r["centroid"]), float(r["flatness"]), float(r["low"]), float(r["mid"]),
                float(r["high"]), float(r["decay"]), float(r["pitched"]),
                float(pk) if pk else 0.0, 1.0 if pk else 0.0, float(r["attack_peaks"]),
                float(r["mixed"]), float(r["truncated"]),
            ]
            rows[r["split"]].append((x, CLASSES.index(r["label"]), r["source"], r["mixed"] == "1" or r["truncated"] == "1", float(pk) if pk else None))
    return rows


def arrays(rs):
    return (
        torch.tensor([r[0] for r in rs], dtype=torch.float32),
        torch.tensor([r[1] for r in rs]),
    )


class Net(nn.Module):
    def __init__(self, mean, std, hidden=256):
        super().__init__()
        self.register_buffer("mean", mean)
        self.register_buffer("std", std)
        self.body = nn.Sequential(
            nn.Linear(len(FEATURES), hidden), nn.ReLU(), nn.Dropout(0.3),
            nn.Linear(hidden, hidden), nn.ReLU(), nn.Dropout(0.3),
            nn.Linear(hidden, len(CLASSES)),
        )

    def prep(self, x):
        x = x.clone()
        x[:, 24] = torch.log2(x[:, 24].clamp(min=20.0))  # centroid
        x[:, 29] = torch.log(x[:, 29].clamp(min=1e-3))  # decay
        return (x - self.mean) / self.std

    def forward(self, x):
        return self.body(self.prep(x))


class Exported(nn.Module):
    """Raw features in; class probabilities and the top probability out."""
    def __init__(self, net):
        super().__init__()
        self.net = net

    def forward(self, features):
        p = torch.softmax(self.net(features), dim=-1)
        return p, p.max(dim=-1).values


def predict(net, x):
    net.eval()
    with torch.no_grad():
        return torch.softmax(net(x), dim=-1)


def report(net, rows, rule_rows=None):
    out = []
    for title, pick in [
        ("One-shots, alone", lambda r: r[2] == "oneshot"),
        ("Loops, all hits", lambda r: r[2] == "loop"),
        ("Loops, clean hits only", lambda r: r[2] == "loop" and not r[3]),
    ]:
        rs = [r for r in rows if pick(r)]
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
    a = ap.parse_args()
    torch.manual_seed(a.seed)
    np.random.seed(a.seed)
    rows = load(a.data)
    xtr, ytr = arrays(rows["train"])
    xva, yva = arrays(rows["val"])
    probe = xtr.clone()
    probe[:, 24] = torch.log2(probe[:, 24].clamp(min=20.0))
    probe[:, 29] = torch.log(probe[:, 29].clamp(min=1e-3))
    mean, std = probe.mean(0), probe.std(0).clamp(min=1e-3)
    for i in (32, 34, 35):  # the flags go in as they are
        mean[i], std[i] = 0.0, 1.0
    net = Net(mean, std)
    counts = torch.bincount(ytr, minlength=len(CLASSES)).float().clamp(min=1)
    weight = (counts.sum() / counts / len(CLASSES)).sqrt()
    loss_fn = nn.CrossEntropyLoss(weight=weight)
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
            loss_fn(net(xb), ytr[idx]).backward()
            opt.step()
        sched.step()
        acc = (predict(net, xva).argmax(1) == yva).float().mean().item()
        if acc > best:
            best, best_state = acc, {k: v.clone() for k, v in net.state_dict().items()}
    net.load_state_dict(best_state)
    os.makedirs(a.out, exist_ok=True)
    net.eval()
    ex = Exported(net).eval()
    torch.onnx.export(
        ex, (torch.zeros(1, len(FEATURES)),), os.path.join(a.out, "classifier.onnx"),
        input_names=["features"], output_names=["probs", "confidence"],
        dynamic_axes={"features": {0: "n"}, "probs": {0: "n"}, "confidence": {0: "n"}},
        opset_version=17, dynamo=False,
    )
    import onnxruntime as ort
    sess = ort.InferenceSession(os.path.join(a.out, "classifier.onnx"))
    xt = arrays(rows["test"])[0][:200]
    got = sess.run(None, {"features": xt.numpy()})[0]
    assert np.allclose(got, predict(net, xt).numpy(), atol=1e-4), "ONNX output differs from PyTorch"
    rep = f"# Learned classifier, test split\n\nBest validation accuracy {100*best:.1f}% over {a.epochs} epochs, seed {a.seed}.\n\n" + report(net, rows["test"])
    open(os.path.join(a.out, "report.md"), "w").write(rep)
    json.dump({"classes": CLASSES, "features": FEATURES, "quiet_db": QUIET_DB}, open(os.path.join(a.out, "io.json"), "w"), indent=1)
    print(rep)


if __name__ == "__main__":
    main()
