use crate::{db, model};
use rust_ann::tensor::Adam;
use rust_ann::text::hashed_ngrams;
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::time::Instant;

const SEED: u64 = 42;
pub const DEFAULT_EPOCHS: usize = 100;
const BATCH: usize = 8;
const LR: f64 = 0.01;

pub const MIN_PER_CATEGORY: usize = 5;

#[derive(Debug)]
pub struct Report {
    pub labels: Vec<String>,
    pub n_train: usize,
    pub n_test: usize,
    pub accuracy: f64,
    pub skipped: Vec<(String, usize)>,
    /// confusion[true][predicted] on the held-out notes, indexed like `labels`.
    pub confusion: Vec<Vec<usize>>,
}

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn shuffle<T>(items: &mut [T], state: &mut u64) {
    for i in (1..items.len()).rev() {
        items.swap(i, (xorshift(state) % (i as u64 + 1)) as usize);
    }
}

/// One-line bar on stderr: `[█████░░░░░] epoch 12/30  loss 0.412  8s  ETA 12s`.
fn progress(epoch: usize, epochs: usize, done: usize, steps: usize, loss: f64, start: Instant) {
    const WIDTH: usize = 30;
    let frac = done as f64 / steps as f64;
    let filled = (frac * WIDTH as f64) as usize;
    let elapsed = start.elapsed().as_secs_f64();
    let eta = elapsed / frac - elapsed;
    eprint!(
        "\r[{}{}] epoch {epoch:>3}/{epochs}  loss {loss:.3}  {elapsed:.0}s  ETA {eta:.0}s  ",
        "█".repeat(filled),
        "░".repeat(WIDTH - filled)
    );
}

/// Trains a TextMlp on the user's manual categories and publishes it as one
/// atomic model.bin (see `model::publish`).
/// Adds `category<TAB>text` lines from `path` to `by_cat`; blank lines are skipped.
fn read_extra(path: &Path, by_cat: &mut BTreeMap<String, Vec<String>>) -> Result<(), String> {
    let data = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    for (i, line) in data.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match line.split_once('\t') {
            Some((cat, text)) if !cat.trim().is_empty() && !text.trim().is_empty() => {
                by_cat.entry(cat.trim().to_string()).or_default().push(text.trim().to_string())
            }
            _ => return Err(format!("{}:{}: expected category<TAB>text", path.display(), i + 1)),
        }
    }
    Ok(())
}

pub fn train(db_path: &Path, model_dir: &Path, extra: Option<&Path>, epochs: usize, now_ms: i64) -> Result<Report, String> {
    let c = db::open_reader(db_path).map_err(|e| e.to_string())?;
    let mut by_cat: BTreeMap<String, Vec<String>> = BTreeMap::new();
    {
        let mut stmt = c
            .prepare("SELECT category_manual, body FROM notes WHERE category_manual IS NOT NULL ORDER BY id")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (cat, body) = row.map_err(|e| e.to_string())?;
            by_cat.entry(cat).or_default().push(body);
        }
    }
    if let Some(path) = extra {
        read_extra(path, &mut by_cat)?;
    }

    let kept: Vec<_> = by_cat.iter().filter(|(_, n)| n.len() >= MIN_PER_CATEGORY).collect();
    if kept.len() < 2 {
        let have = if by_cat.is_empty() {
            "none".to_string()
        } else {
            by_cat.iter().map(|(c, n)| format!("{c} ({})", n.len())).collect::<Vec<_>>().join(", ")
        };
        return Err(format!("need at least 2 categories with 5+ labeled notes; have: {have}"));
    }
    let skipped = by_cat
        .iter()
        .filter(|(_, n)| n.len() < MIN_PER_CATEGORY)
        .map(|(c, n)| (c.clone(), n.len()))
        .collect();
    let labels: Vec<String> = kept.iter().map(|(c, _)| (*c).clone()).collect();

    let mut rng = SEED;
    let (mut train_set, mut test_set) = (Vec::new(), Vec::new());
    for (label, (_, bodies)) in kept.iter().enumerate() {
        let mut examples: Vec<_> = bodies.iter().map(|b| (hashed_ngrams(b, model::BUCKETS), label)).collect();
        shuffle(&mut examples, &mut rng);
        let n_test = (examples.len() / 5).max(1);
        train_set.extend(examples.split_off(n_test));
        test_set.extend(examples);
    }

    let io = |e: std::io::Error| format!("{}: {e}", model_dir.display());
    std::fs::create_dir_all(model_dir).map_err(io)?;
    // every run is appended to <model dir>/train.log; log write failures never stop training
    let mut log = OpenOptions::new().create(true).append(true).open(model_dir.join("train.log")).map_err(io)?;
    let _ = writeln!(
        log,
        "== run {now_ms}: train {} test {} | labels {} | epochs {epochs} batch {BATCH} lr {LR} buckets {}",
        train_set.len(),
        test_set.len(),
        labels.join(","),
        model::BUCKETS
    );

    let (mut tape, mlp) = model::new_model(labels.len(), SEED);
    let mut adam = Adam::new(&tape, LR);
    // ponytail: progress bar only on a terminal, so tests and pipes stay quiet
    let show = std::io::stderr().is_terminal();
    let (start, steps) = (Instant::now(), epochs * train_set.len().div_ceil(BATCH));
    let mut done = 0;
    for epoch in 1..=epochs {
        shuffle(&mut train_set, &mut rng);
        let mut loss_sum = 0.0;
        for (i, batch) in train_set.chunks(BATCH).enumerate() {
            loss_sum += mlp.train_batch(&mut tape, &mut adam, batch);
            done += 1;
            if show {
                progress(epoch, epochs, done, steps, loss_sum / (i + 1) as f64, start);
            }
        }
        let batches = train_set.len().div_ceil(BATCH);
        let _ = writeln!(
            log,
            "epoch {epoch:>3}/{epochs}  loss {:.4}  {:.1}s",
            loss_sum / batches as f64,
            start.elapsed().as_secs_f64()
        );
    }
    if show {
        eprintln!();
    }
    // confusion[true][predicted] on the held-out notes
    let mut confusion: Vec<Vec<usize>> = vec![vec![0; labels.len()]; labels.len()];
    for (ids, label) in &test_set {
        let p = mlp.predict(&mut tape, ids);
        let guess = (0..p.len()).max_by(|&a, &b| p[a].total_cmp(&p[b])).unwrap_or(0);
        confusion[*label][guess] += 1;
    }
    let correct: usize = (0..labels.len()).map(|i| confusion[i][i]).sum();
    let _ = writeln!(
        log,
        "accuracy {:.1}% ({correct}/{})  [rows = true, columns = predicted: {}]",
        100.0 * correct as f64 / test_set.len() as f64,
        test_set.len(),
        labels.join(" ")
    );
    for (label, row) in labels.iter().zip(&confusion) {
        let _ = writeln!(log, "  {label:>12} {row:?}");
    }

    model::publish(model_dir, &tape, &labels, now_ms as u64).map_err(io)?;

    Ok(Report {
        labels,
        n_train: train_set.len(),
        n_test: test_set.len(),
        accuracy: correct as f64 / test_set.len() as f64,
        skipped,
        confusion,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn label(c: &Connection, cat: &str, texts: &[String]) {
        for t in texts {
            let id = db::insert_note(c, t, 0).unwrap();
            db::set_manual_category(c, id, Some(cat)).unwrap();
        }
    }
    fn notes(prefix: &str, n: usize) -> Vec<String> {
        (0..n).map(|i| format!("{prefix} {i}")).collect()
    }

    #[test]
    fn too_few_labels_errors_and_writes_nothing() {
        let (d, p, c) = db::test_db();
        label(&c, "kerja", &notes("rapat tim proyek", 5));
        label(&c, "belanja", &notes("beli sayur pasar", 4));
        let dir = d.path().join("model");
        let e = train(&p, &dir, None, 30, 1).err().unwrap();
        assert_eq!(e, "need at least 2 categories with 5+ labeled notes; have: belanja (4), kerja (5)");
        assert!(!dir.exists());
    }

    #[test]
    fn no_labels_says_none() {
        let (d, p, _c) = db::test_db();
        let e = train(&p, &d.path().join("model"), None, 30, 1).err().unwrap();
        assert_eq!(e, "need at least 2 categories with 5+ labeled notes; have: none");
    }

    #[test]
    fn trains_and_writes_loadable_model() {
        let (d, p, c) = db::test_db();
        label(&c, "kerja", &notes("rapat tim proyek", 6));
        label(&c, "belanja", &notes("beli sayur pasar", 6));
        label(&c, "misc", &notes("random", 2));
        let dir = d.path().join("model");
        let r = train(&p, &dir, None, 30, 777).unwrap();
        assert_eq!((r.labels.clone(), r.n_train, r.n_test), (vec!["belanja".to_string(), "kerja".into()], 10, 2));
        assert_eq!(r.skipped, vec![("misc".to_string(), 2)]);
        assert_eq!(model::read_version(&dir).as_deref(), Some("777"));
        let (mut f, _) = model::load(&dir, 0.0).unwrap();
        assert_eq!(f("beli sayur pasar 1").unwrap().as_deref(), Some("belanja"));
        assert_eq!(r.confusion.iter().flatten().sum::<usize>(), r.n_test);
        let log = std::fs::read_to_string(dir.join("train.log")).unwrap();
        assert!(log.contains("epoch  30/30") && log.contains("accuracy") && log.contains("belanja"), "{log}");
    }

    #[test]
    fn extra_tsv_adds_examples() {
        let (d, p, c) = db::test_db();
        label(&c, "kerja", &notes("rapat tim proyek", 6));
        let tsv = d.path().join("extra.tsv");
        let lines: Vec<String> = notes("beli sayur pasar", 6).iter().map(|t| format!("belanja\t{t}")).collect();
        std::fs::write(&tsv, lines.join("\n") + "\n\n").unwrap();
        let r = train(&p, &d.path().join("model"), Some(&tsv), 30, 1).unwrap();
        assert_eq!(r.labels, ["belanja", "kerja"]);
        assert_eq!(r.n_train + r.n_test, 12);
    }

    #[test]
    fn bad_extra_line_names_the_line() {
        let (d, p, _c) = db::test_db();
        let tsv = d.path().join("extra.tsv");
        std::fs::write(&tsv, "idea\tbikin fitur export\nno tab on this line\n").unwrap();
        let e = train(&p, &d.path().join("model"), Some(&tsv), 30, 1).err().unwrap();
        assert!(e.contains("extra.tsv:2:"), "{e}");
    }

    #[test]
    fn training_is_repeatable() {
        let (d, p, c) = db::test_db();
        label(&c, "kerja", &notes("rapat tim proyek", 6));
        label(&c, "belanja", &notes("beli sayur pasar", 6));
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        train(&p, &a, None, 30, 1).unwrap();
        train(&p, &b, None, 30, 1).unwrap();
        assert_eq!(std::fs::read(a.join("model.bin")).unwrap(), std::fs::read(b.join("model.bin")).unwrap());
    }
}
