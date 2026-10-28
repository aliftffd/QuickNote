use crate::{db, model};
use rust_ann::tensor::Adam;
use rust_ann::text::hashed_ngrams;
use std::collections::BTreeMap;
use std::path::Path;

const SEED: u64 = 42;
const EPOCHS: usize = 30;
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

/// Trains a TextMlp on the user's manual categories and publishes it as one
/// atomic model.bin (see `model::publish`).
pub fn train(db_path: &Path, model_dir: &Path, now_ms: i64) -> Result<Report, String> {
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

    let (mut tape, mlp) = model::new_model(labels.len(), SEED);
    let mut adam = Adam::new(&tape, LR);
    for _ in 0..EPOCHS {
        shuffle(&mut train_set, &mut rng);
        for batch in train_set.chunks(BATCH) {
            mlp.train_batch(&mut tape, &mut adam, batch);
        }
    }
    let correct = test_set
        .iter()
        .filter(|(ids, label)| {
            let p = mlp.predict(&mut tape, ids);
            (0..p.len()).max_by(|&a, &b| p[a].total_cmp(&p[b])) == Some(*label)
        })
        .count();

    model::publish(model_dir, &tape, &labels, now_ms as u64).map_err(|e| format!("{}: {e}", model_dir.display()))?;

    Ok(Report {
        labels,
        n_train: train_set.len(),
        n_test: test_set.len(),
        accuracy: correct as f64 / test_set.len() as f64,
        skipped,
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
        let e = train(&p, &dir, 1).err().unwrap();
        assert_eq!(e, "need at least 2 categories with 5+ labeled notes; have: belanja (4), kerja (5)");
        assert!(!dir.exists());
    }

    #[test]
    fn no_labels_says_none() {
        let (d, p, _c) = db::test_db();
        let e = train(&p, &d.path().join("model"), 1).err().unwrap();
        assert_eq!(e, "need at least 2 categories with 5+ labeled notes; have: none");
    }

    #[test]
    fn trains_and_writes_loadable_model() {
        let (d, p, c) = db::test_db();
        label(&c, "kerja", &notes("rapat tim proyek", 6));
        label(&c, "belanja", &notes("beli sayur pasar", 6));
        label(&c, "misc", &notes("random", 2));
        let dir = d.path().join("model");
        let r = train(&p, &dir, 777).unwrap();
        assert_eq!((r.labels.clone(), r.n_train, r.n_test), (vec!["belanja".to_string(), "kerja".into()], 10, 2));
        assert_eq!(r.skipped, vec![("misc".to_string(), 2)]);
        assert_eq!(model::read_version(&dir).as_deref(), Some("777"));
        let (mut f, _) = model::load(&dir, 0.0).unwrap();
        assert_eq!(f("beli sayur pasar 1").unwrap().as_deref(), Some("belanja"));
    }

    #[test]
    fn training_is_repeatable() {
        let (d, p, c) = db::test_db();
        label(&c, "kerja", &notes("rapat tim proyek", 6));
        label(&c, "belanja", &notes("beli sayur pasar", 6));
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        train(&p, &a, 1).unwrap();
        train(&p, &b, 1).unwrap();
        assert_eq!(std::fs::read(a.join("model.bin")).unwrap(), std::fs::read(b.join("model.bin")).unwrap());
    }
}
