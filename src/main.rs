mod ai;
mod app;
mod db;
mod editor;
mod model;
mod recovery;
mod save;
mod train;
mod ui;

use app::App;
use ratatui::crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind};
use ratatui::crossterm::execute;
use std::error::Error;
use std::fs::OpenOptions;
use std::io::{stdout, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{channel, sync_channel, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// `$XDG_DATA_HOME/quicknote`, falling back to `~/.local/share/quicknote`.
fn data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"));
    base.join("quicknote")
}

fn main() {
    let dir = data_dir();
    let result = std::fs::create_dir_all(&dir).map_err(Into::into).and_then(|()| {
        if std::env::args().nth(1).as_deref() == Some("train") {
            run_train(&dir)
        } else {
            run_app(&dir)
        }
    });
    if let Err(e) = result {
        eprintln!("quicknote: {e}");
        std::process::exit(1);
    }
}

fn run_train(dir: &Path) -> Result<(), Box<dyn Error>> {
    let db_path = dir.join("notes.db");
    db::open(&db_path)?; // create/migrate so training works before the first launch
    let r = train::train(&db_path, &dir.join("model"), db::now_ms())?;
    println!(
        "trained on {} notes, tested on {}: accuracy {:.1}%",
        r.n_train,
        r.n_test,
        r.accuracy * 100.0
    );
    println!("categories: {}", r.labels.join(", "));
    for (cat, n) in r.skipped {
        println!("skipped {cat} ({n} notes, need {})", train::MIN_PER_CATEGORY);
    }
    Ok(())
}

/// Last active note if it still exists, else the newest, else a fresh empty one.
fn start_note(c: &rusqlite::Connection) -> rusqlite::Result<db::Note> {
    let remembered = db::kv_get(c, "active_note_id")?.and_then(|v| v.parse().ok());
    for id in [remembered, db::newest_note_id(c)?].into_iter().flatten() {
        if let Some(n) = db::load_note(c, id)? {
            return Ok(n);
        }
    }
    let id = db::insert_note(c, "", db::now_ms())?;
    Ok(db::load_note(c, id)?.expect("just inserted"))
}

fn run_app(dir: &Path) -> Result<(), Box<dyn Error>> {
    let db_path = dir.join("notes.db");
    let writer = db::open(&db_path)?;
    db::delete_empty_notes(&writer)?;
    let note = start_note(&writer)?;
    let reader = db::open_reader(&db_path)?;

    let (ai_wake_tx, ai_wake_rx) = sync_channel(1);
    let (tx, replies, save_thread) = save::spawn(writer, ai_wake_tx);
    let (ai_status_tx, ai_status_rx) = channel();
    let active = Arc::new(AtomicI64::new(note.id));
    // not joined on exit: AI writes are single transactions, safe to abandon mid-inference
    ai::spawn(db_path, dir.join("model"), ai_wake_rx, ai_status_tx, active.clone());

    let signaled = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, signaled.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGHUP, signaled.clone())?;

    let mut latency = match std::env::var_os("QUICKNOTE_LATENCY_LOG") {
        Some(p) => Some(BufWriter::new(OpenOptions::new().create(true).append(true).open(p)?)),
        None => None,
    };

    let mut app = App::new(reader, tx, note, dir.join("recovery"), active);
    let mut terminal = ratatui::init(); // also installs a panic hook that restores the terminal

    // crossterm's poll/read spin forever (ignoring timeouts) once the terminal hangs up,
    // so input lives on its own thread: the main loop keeps running, sees SIGHUP, saves, exits.
    let (input_tx, input) = channel();
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if input_tx.send(ev).is_err() {
                return;
            }
        }
    });
    let _ = execute!(stdout(), EnableBracketedPaste);

    let mut signal_handled = false;
    let result = (|| -> Result<(), Box<dyn Error>> {
        loop {
            let mut started = None;
            match input.recv_timeout(Duration::from_millis(50)) {
                Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => {
                    let now = Instant::now();
                    app.on_key(k, now);
                    started = Some(now);
                }
                Ok(Event::Paste(s)) => {
                    let now = Instant::now();
                    app.on_paste(&s, now);
                    started = Some(now);
                }
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                // the input thread stopped: the terminal is gone
                Err(RecvTimeoutError::Disconnected) => signaled.store(true, Ordering::Relaxed),
            }
            let now = Instant::now();
            while let Ok(r) = replies.try_recv() {
                app.on_reply(r, now);
            }
            while let Ok(s) = ai_status_rx.try_recv() {
                app.ai_status = s;
            }
            if !signal_handled && signaled.load(Ordering::Relaxed) {
                signal_handled = true;
                app.on_signal();
            }
            app.tick(now);
            if terminal.draw(|f| ui::draw(f, &app)).is_err() {
                // terminal is unusable: take the signal path so the buffer still gets saved
                signaled.store(true, Ordering::Relaxed);
            }
            if let (Some(t), Some(log)) = (started, latency.as_mut()) {
                if writeln!(log, "{}", t.elapsed().as_micros()).is_err() {
                    latency = None; // diagnostics must never take the editor down
                    app.message = Some("latency log write failed; logging disabled".into());
                }
            }
            if app.should_exit() {
                return Ok(());
            }
        }
    })();

    let _ = execute!(stdout(), DisableBracketedPaste);
    ratatui::restore();
    if let Some(log) = latency.as_mut() {
        let _ = log.flush();
    }
    let fatal = app.fatal.take();
    drop(app); // drops the last WriteReq sender, so the save thread drains and exits
    let _ = save_thread.join();
    match fatal {
        Some(msg) => Err(msg.into()),
        None => result,
    }
}
