use crate::ai::Classify;
use rust_ann::layers::TextMlp;
use rust_ann::tensor::Tape;
use rust_ann::text::hashed_ngrams;
use std::fs::File;
use std::io::{Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

pub const BUCKETS: usize = 65_536;
pub const DIM: usize = 32;
pub const HIDDEN: usize = 32;
pub const NO_MODEL: &str = "no model yet, run quicknote train";

/// The one architecture QuickNote trains and loads; shapes must match `model.bin`.
pub fn new_model(n_classes: usize, seed: u64) -> (Tape, TextMlp) {
    let mut tape = Tape::new();
    let mut seed = seed;
    let model = TextMlp::new(&mut tape, BUCKETS, DIM, HIDDEN, n_classes, &mut seed);
    tape.freeze_params();
    (tape, model)
}

const FILE: &str = "model.bin";

/// The model's version: the first 8 bytes of model.bin (cheap to poll).
pub fn read_version(dir: &Path) -> Option<String> {
    let mut head = [0u8; 8];
    std::fs::File::open(dir.join(FILE)).ok()?.read_exact(&mut head).ok()?;
    Some(u64::from_le_bytes(head).to_string())
}

/// Writes one self-contained model.bin — `[u64 version][u32 len][labels, '\n'-joined][weights]` —
/// via a per-process temp file and a single rename, so readers and concurrent
/// trainers only ever see a complete, consistent model.
pub fn publish(dir: &Path, tape: &Tape, labels: &[String], version: u64) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let labels = labels.join("\n");
    let mut bytes = version.to_le_bytes().to_vec();
    bytes.extend((labels.len() as u32).to_le_bytes());
    bytes.extend(labels.as_bytes());
    tape.save_params(&mut bytes)?;
    let tmp = dir.join(format!(".{FILE}.{}.tmp", std::process::id()));
    let mut f = File::create(&tmp)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, dir.join(FILE))?;
    File::open(dir)?.sync_all()
}

/// `QUICKNOTE_MIN_CONFIDENCE`, default 0.7: below it a note stays Uncategorized.
// ponytail: 0.7, not 0.5 — with 2 categories the top probability is always ≥ 0.5, so 0.5 never fires
pub fn min_confidence() -> f64 {
    std::env::var("QUICKNOTE_MIN_CONFIDENCE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.7)
}

pub fn load(dir: &Path, min_confidence: f64) -> Result<(Classify, String), String> {
    let bytes = match std::fs::read(dir.join(FILE)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(NO_MODEL.into()),
        Err(e) => return Err(format!("{FILE}: {e}")),
    };
    let bad = || format!("{FILE}: not a QuickNote model");
    let version = u64::from_le_bytes(bytes.get(..8).ok_or_else(bad)?.try_into().unwrap()).to_string();
    let len = u32::from_le_bytes(bytes.get(8..12).ok_or_else(bad)?.try_into().unwrap()) as usize;
    let labels: Vec<String> = std::str::from_utf8(bytes.get(12..12 + len).ok_or_else(bad)?)
        .map_err(|_| bad())?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    if labels.len() < 2 {
        return Err(format!("{FILE}: need at least 2 categories"));
    }
    let (mut tape, model) = new_model(labels.len(), 0);
    tape.load_params(&mut &bytes[12 + len..]).map_err(|e| format!("{FILE}: {e}"))?;

    let classify: Classify = Box::new(move |text| {
        let result = catch_unwind(AssertUnwindSafe(|| model.predict(&mut tape, &hashed_ngrams(text, BUCKETS))));
        let probs = result.map_err(|_| {
            tape.reset(); // a panic mid-forward leaves tensors on the tape
            "inference panicked".to_string()
        })?;
        let (best, p) = probs
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .ok_or("model produced no output")?;
        Ok((*p >= min_confidence).then(|| labels[best].clone()))
    });
    Ok((classify, version))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_model(dir: &Path, labels: &[&str], version: u64) {
        let (t, _m) = new_model(labels.len(), 1);
        let labels: Vec<String> = labels.iter().map(|l| l.to_string()).collect();
        publish(dir, &t, &labels, version).unwrap();
    }

    #[test]
    fn one_file_carries_labels_and_version() {
        let d = tempfile::tempdir().unwrap();
        write_model(d.path(), &["a", "b"], 1);
        write_model(d.path(), &["c", "d"], 2);
        let (mut f, v) = load(d.path(), 0.0).unwrap();
        assert_eq!((v.as_str(), read_version(d.path()).as_deref()), ("2", Some("2")));
        assert!(matches!(f("x").unwrap().as_deref(), Some("c") | Some("d")));
        let names: Vec<_> = std::fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["model.bin"]);
    }

    #[test]
    fn missing_model_says_run_train() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(load(&d.path().join("model"), 0.5).err().unwrap(), "no model yet, run quicknote train");
    }

    #[test]
    fn loads_and_classifies() {
        let d = tempfile::tempdir().unwrap();
        write_model(d.path(), &["belanja", "kerja"], 123);
        let (mut f, v) = load(d.path(), 0.0).unwrap();
        assert_eq!(v, "123");
        assert!(matches!(f("rapat besok").unwrap().as_deref(), Some("belanja") | Some("kerja")));
    }

    #[test]
    fn below_cutoff_is_none() {
        let d = tempfile::tempdir().unwrap();
        write_model(d.path(), &["a", "b"], 1);
        let (mut f, _) = load(d.path(), 1.01).unwrap();
        assert_eq!(f("apa saja").unwrap(), None);
    }

    #[test]
    fn model_trained_for_other_labels_is_rejected() {
        let d = tempfile::tempdir().unwrap();
        let (t, _m) = new_model(2, 1);
        publish(d.path(), &t, &["a".into(), "b".into(), "c".into()], 1).unwrap();
        assert!(load(d.path(), 0.5).err().unwrap().starts_with("model.bin:"));
    }
}
