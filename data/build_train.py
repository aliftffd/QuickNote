"""Build data/train.tsv for `quicknote train --extra data/train.tsv`.

usage: python3 data/build_train.py [wiki_dataset.zip]   (from the project root)

Classes: idea, kerja, meeting, research.
- generated.tsv (Claude) + codex_generated.tsv (Codex): hand-written mixed Indonesian/English
  notes for all four classes, two authors so the model learns classes rather than one style.
- wiki_dataset.zip (optional): research folders of the wiki vault become `research`,
  trimmed to note length so the model can't learn "long English text = research".
Every class is capped at PER_CLASS examples so the classes stay balanced.
"""
import collections, json, random, re, sys, zipfile
from pathlib import Path

HERE = Path(__file__).parent
PER_CLASS = 200
RESEARCH_DIRS = {"papers", "concepts", "theory", "datasets", "questions", "architecture"}
NOTE_CHARS = 300  # ponytail: first 300 chars of a section ≈ a quick note; tune if notes get longer


def clean(text):
    """Markdown → plain one-line text: drop wiki links, callouts, headings and the TSV-breaking chars."""
    text = re.sub(r"\[\[([^\]|]+)(\|[^\]]+)?\]\]", r"\1", text)
    text = re.sub(r"> ?\[![^\]]+\]", " ", text)
    text = re.sub(r"[#*>`|_]+", " ", text)
    text = " ".join(text.split())
    if len(text) > NOTE_CHARS:
        text = text[:NOTE_CHARS].rsplit(" ", 1)[0]
    return text


def main():
    rng = random.Random(42)
    pool = collections.defaultdict(list)
    for name in ("generated.tsv", "codex_generated.tsv"):
        for line in (HERE / name).read_text(encoding="utf-8").splitlines():
            cat, text = line.split("\t", 1)
            pool[cat].append(text)
    generated_research = len(pool["research"])

    if len(sys.argv) > 1:
        with zipfile.ZipFile(sys.argv[1]) as z:
            rows = [json.loads(l) for l in z.read("wiki_clean.jsonl").decode("utf-8").splitlines()]
        wiki = [clean(r["text"]) for r in rows if r["dir"] in RESEARCH_DIRS and not r["retraction"]]
        wiki = [t for t in wiki if len(t) >= 40]
        rng.shuffle(wiki)
        # hand-written research notes first, wiki fills the rest of the class
        pool["research"] += wiki[: max(0, PER_CLASS - generated_research)]

    out = []
    for cat in sorted(pool):
        items = pool[cat][:PER_CLASS]
        out += [f"{cat}\t{t}" for t in items]
    (HERE / "train.tsv").write_text("\n".join(out) + "\n", encoding="utf-8")

    counts = collections.Counter(l.split("\t")[0] for l in out)
    assert all("\t" not in l.split("\t", 1)[1] and "\n" not in l for l in out)
    assert max(counts.values()) - min(counts.values()) <= PER_CLASS // 2, f"unbalanced: {counts}"
    print(f"wrote {HERE / 'train.tsv'}: {dict(counts)}")


if __name__ == "__main__":
    main()
