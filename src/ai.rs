use crate::app::AiStatus;
use crate::{db, model};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// `Ok(None)` = below the confidence cutoff: no AI category, but the note counts as processed.
pub type Classify = Box<dyn FnMut(&str) -> Result<Option<String>, String> + Send>;

/// Classifies at most one note; returns its id (even if the result was stale and dropped).
pub fn step(c: &Connection, f: &mut Classify, model: &str, active: Option<i64>, now_ms: i64) -> rusqlite::Result<Option<i64>> {
    let Some((id, body, rev)) = db::next_ai_job(c, model, active, now_ms)? else { return Ok(None) };
    // ponytail: failed notes wait for next edit or model bump; add retry counter if models fail transiently
    let category = f(&body).unwrap_or(None);
    db::write_ai_result(c, id, rev, category.as_deref(), model)?;
    Ok(Some(id))
}

pub fn run(
    db_path: &Path,
    wake: Receiver<()>,
    status: Sender<AiStatus>,
    active: Arc<AtomicI64>,
    mut load: impl FnMut() -> Result<(Classify, String), String>,
    mut peek_version: impl FnMut() -> Option<String>,
    retry: Duration,
) {
    let conn = match db::open(db_path) {
        Ok(c) => c,
        Err(e) => {
            let _ = status.send(AiStatus::Paused(e.to_string()));
            return;
        }
    };
    let mut model: Option<(Classify, String)> = None;
    'outer: loop {
        if let Some((_, loaded)) = &model {
            if peek_version().is_some_and(|v| &v != loaded) {
                model = None; // a newer model was trained
            }
        }
        if model.is_none() {
            match load() {
                Ok(m) => model = Some(m),
                Err(e) => {
                    let _ = status.send(AiStatus::Paused(e));
                }
            }
        }
        if let Some((classify, version)) = &mut model {
            let _ = status.send(AiStatus::Working);
            loop {
                if peek_version().is_some_and(|v| &v != version) {
                    continue 'outer; // newer model published: reload before writing more results
                }
                let a = active.load(Ordering::Relaxed);
                match step(&conn, classify, version, (a > 0).then_some(a), db::now_ms()) {
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(e) => {
                        let _ = status.send(AiStatus::Paused(e.to_string()));
                        break;
                    }
                }
            }
            let _ = status.send(AiStatus::Idle);
        }
        let timeout = if model.is_none() { retry } else { Duration::from_secs(30) };
        match wake.recv_timeout(timeout) {
            Ok(()) => {
                // let the just-saved note pass the 4 s filter, and coalesce bursts of saves
                std::thread::sleep(Duration::from_secs(4));
                while wake.try_recv().is_ok() {}
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

pub fn spawn(db_path: PathBuf, model_dir: PathBuf, wake: Receiver<()>, status: Sender<AiStatus>, active: Arc<AtomicI64>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        run(
            &db_path,
            wake,
            status,
            active,
            || model::load(&model_dir, model::min_confidence()),
            || model::read_version(&model_dir),
            Duration::from_secs(60),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Mutex};

    fn fixed(cat: &'static str) -> Classify {
        Box::new(move |_| Ok(Some(cat.into())))
    }
    fn category_ai(c: &Connection, id: i64) -> Option<String> {
        c.query_row("SELECT category_ai FROM notes WHERE id=?", [id], |r| r.get(0)).unwrap()
    }

    #[test]
    fn active_first_and_recent_skipped() {
        let (_d, _p, c) = db::test_db();
        let old = db::insert_note(&c, "old", 0).unwrap();
        let act = db::insert_note(&c, "act", 0).unwrap();
        let fresh = db::insert_note(&c, "fresh", 9_000).unwrap();
        let mut f = fixed("x");
        assert_eq!(step(&c, &mut f, "v1", Some(act), 10_000).unwrap(), Some(act));
        assert_eq!(step(&c, &mut f, "v1", Some(act), 10_000).unwrap(), Some(old));
        assert_eq!(step(&c, &mut f, "v1", Some(act), 10_000).unwrap(), None); // fresh is < 4 s old
        assert_eq!(step(&c, &mut f, "v1", Some(act), 13_000).unwrap(), Some(fresh));
    }

    #[test]
    fn stale_result_dropped_and_redone() {
        let (_d, _p, c) = db::test_db();
        let id = db::insert_note(&c, "a", 0).unwrap();
        let (_, _, rev) = db::next_ai_job(&c, "v1", None, 10_000).unwrap().unwrap();
        db::save_note(&c, id, 0, "b", 0, 1).unwrap();
        assert!(!db::write_ai_result(&c, id, rev, Some("x"), "v1").unwrap());
        assert_eq!(db::next_ai_job(&c, "v1", None, 10_000).unwrap().unwrap().2, 1);
    }

    #[test]
    fn manual_never_overwritten_and_model_change_reprocesses() {
        let (_d, _p, c) = db::test_db();
        let m = db::insert_note(&c, "m", 0).unwrap();
        let a = db::insert_note(&c, "a", 0).unwrap();
        db::set_manual_category(&c, m, Some("mine")).unwrap();
        let mut f = fixed("x");
        while step(&c, &mut f, "v1", None, 10_000).unwrap().is_some() {}
        assert!(!db::write_ai_result(&c, m, 0, Some("x"), "v1").unwrap());
        assert_eq!(step(&c, &mut f, "v2", None, 10_000).unwrap(), Some(a));
        assert_eq!(step(&c, &mut f, "v2", None, 10_000).unwrap(), None);
    }

    #[test]
    fn classify_error_marks_note_and_moves_on() {
        let (_d, _p, c) = db::test_db();
        let bad = db::insert_note(&c, "bad", 0).unwrap();
        let good = db::insert_note(&c, "good", 0).unwrap();
        let mut f: Classify = Box::new(|t| if t == "bad" { Err("too long".into()) } else { Ok(Some("x".into())) });
        let mut seen = vec![];
        while let Some(id) = step(&c, &mut f, "v1", None, 10_000).unwrap() {
            seen.push(id);
        }
        seen.sort();
        assert_eq!(seen, [bad, good]);
    }

    #[test]
    fn below_cutoff_is_processed_without_category() {
        let (_d, _p, c) = db::test_db();
        let id = db::insert_note(&c, "a", 0).unwrap();
        let mut f: Classify = Box::new(|_| Ok(None));
        assert_eq!(step(&c, &mut f, "v1", None, 10_000).unwrap(), Some(id));
        assert_eq!(step(&c, &mut f, "v1", None, 10_000).unwrap(), None);
    }

    #[test]
    fn run_pauses_then_recovers_when_model_appears() {
        let (_d, p, c) = db::test_db();
        let id = db::insert_note(&c, "a", 0).unwrap();
        let (wtx, wrx) = mpsc::channel();
        let (stx, srx) = mpsc::channel();
        let mut tries = 0;
        let h = std::thread::spawn(move || {
            run(
                &p,
                wrx,
                stx,
                Arc::new(AtomicI64::new(0)),
                move || {
                    tries += 1;
                    if tries == 1 { Err("no model yet, run quicknote train".into()) } else { Ok((fixed("x"), "v1".into())) }
                },
                || None,
                Duration::from_millis(10),
            )
        });
        assert_eq!(srx.recv().unwrap(), AiStatus::Paused("no model yet, run quicknote train".into()));
        while srx.recv().unwrap() != AiStatus::Idle {}
        assert_eq!(category_ai(&c, id).as_deref(), Some("x"));
        drop(wtx);
        h.join().unwrap();
    }

    #[test]
    fn run_reloads_when_version_changes() {
        // takes ~4 s: one wake debounce
        let (_d, p, c) = db::test_db();
        let id = db::insert_note(&c, "a", 0).unwrap();
        let (wtx, wrx) = mpsc::channel();
        let (stx, srx) = mpsc::channel();
        let ver = Arc::new(Mutex::new("v1".to_string()));
        let (vl, vp) = (ver.clone(), ver.clone());
        let h = std::thread::spawn(move || {
            run(
                &p,
                wrx,
                stx,
                Arc::new(AtomicI64::new(0)),
                move || {
                    let v = vl.lock().unwrap().clone();
                    Ok((fixed(if v == "v1" { "x" } else { "y" }), v))
                },
                move || Some(vp.lock().unwrap().clone()),
                Duration::from_millis(10),
            )
        });
        while srx.recv().unwrap() != AiStatus::Idle {}
        *ver.lock().unwrap() = "v2".into();
        wtx.send(()).unwrap();
        while srx.recv().unwrap() != AiStatus::Idle {}
        assert_eq!(category_ai(&c, id).as_deref(), Some("y"));
        drop(wtx);
        h.join().unwrap();
    }
}
