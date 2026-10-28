use crate::db::{self, ListItem, Note};
use crate::editor::{Autosave, Buffer};
use crate::recovery;
use crate::save::{WriteReply, WriteReq};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const LIMIT_MSG: &str = "Note is limited to 512,000 bytes; edit rejected";
const POLL: Duration = Duration::from_millis(500);

#[derive(Debug, PartialEq)]
pub enum Status {
    Unsaved,
    Saving,
    Saved,
    Conflict,
    Error(String),
}

#[derive(Debug, PartialEq)]
pub enum AiStatus {
    Idle,
    Working,
    Paused(String),
}

pub enum Mode {
    Edit,
    Conflict,
    QuitError,
    QuitConfirm,
    List { sel: usize },
    Search { query: Buffer, sel: usize },
    CategoryPrompt { input: Buffer },
}

enum PendingInsert {
    SaveAsNew { gen: u64 },
    New,
    Import(PathBuf, String),
}

/// Input that arrives while a new note is being inserted; replayed once it exists.
enum Deferred {
    Key(KeyEvent),
    Paste(String),
    Signal,
}

enum Target {
    Note(i64),
    New,
    Import(PathBuf),
}

pub struct App {
    pub buf: Buffer,
    pub note_id: i64,
    pub mode: Mode,
    pub message: Option<String>,
    pub ai_status: AiStatus,
    pub recovery_dir: PathBuf,
    /// Note list overlay contents, refreshed when the list opens.
    pub list: Vec<ListItem>,
    pub recovered: Vec<PathBuf>,
    /// Search overlay results: (note id, snippet).
    pub results: Vec<(i64, String)>,
    base_rev: i64,
    saved_gen: u64,
    autosave: Autosave,
    in_flight: Option<u64>,
    error: Option<String>,
    quitting: bool,
    forced: bool,
    exit: bool,
    tx: Sender<WriteReq>,
    reader: Connection,
    active: Arc<AtomicI64>,
    last_poll: Instant,
    last_data_version: i64,
    pending_insert: Option<PendingInsert>,
    pending_switch: Option<Target>,
    deferred: Vec<Deferred>,
    /// Set when text could not be preserved on a forced exit; main reports it.
    pub fatal: Option<String>,
}

impl App {
    pub fn new(reader: Connection, tx: Sender<WriteReq>, note: Note, recovery_dir: PathBuf, active: Arc<AtomicI64>) -> App {
        let last_data_version = db::data_version(&reader).unwrap_or(0);
        active.store(note.id, Ordering::Relaxed);
        App {
            buf: Buffer::new(note.body, note.cursor),
            note_id: note.id,
            mode: Mode::Edit,
            message: None,
            ai_status: AiStatus::Idle,
            recovery_dir,
            list: Vec::new(),
            recovered: Vec::new(),
            results: Vec::new(),
            base_rev: note.rev,
            saved_gen: 0,
            autosave: Autosave::default(),
            in_flight: None,
            error: None,
            quitting: false,
            forced: false,
            exit: false,
            tx,
            reader,
            active,
            last_poll: Instant::now(),
            last_data_version,
            pending_insert: None,
            pending_switch: None,
            deferred: Vec::new(),
            fatal: None,
        }
    }

    fn dirty(&self) -> bool {
        self.buf.gen() != self.saved_gen
    }

    pub fn status(&self) -> Status {
        if matches!(self.mode, Mode::Conflict) {
            Status::Conflict
        } else if self.in_flight.is_some() {
            Status::Saving
        } else if let Some(e) = &self.error {
            Status::Error(e.clone())
        } else if self.dirty() {
            Status::Unsaved
        } else {
            Status::Saved
        }
    }

    pub fn should_exit(&self) -> bool {
        self.exit
    }

    fn send(&mut self, req: WriteReq) {
        if self.tx.send(req).is_err() {
            self.error = Some("save thread stopped".into());
        }
    }

    fn send_save(&mut self) {
        if self.in_flight.is_some() {
            return;
        }
        let gen = self.buf.gen();
        self.in_flight = Some(gen);
        self.autosave.on_sent();
        self.send(WriteReq::Save {
            note_id: self.note_id,
            expected_rev: self.base_rev,
            body: self.buf.text().to_string(),
            cursor: self.buf.cursor(),
            gen,
        });
    }

    /// Runs a buffer edit and feeds autosave if the text actually changed.
    fn edit(&mut self, now: Instant, f: impl FnOnce(&mut Buffer)) {
        let before = self.buf.gen();
        f(&mut self.buf);
        if self.buf.gen() != before {
            self.autosave.on_edit(now);
        }
    }

    fn insert(&mut self, s: &str, now: Instant) {
        let mut rejected = false;
        self.edit(now, |b| rejected = b.insert_str(s).is_err());
        if rejected {
            self.message = Some(LIMIT_MSG.into());
        }
    }

    fn load_into(&mut self, note: Note) {
        self.buf = Buffer::new(note.body, note.cursor);
        self.note_id = note.id;
        self.base_rev = note.rev;
        self.saved_gen = 0;
        self.autosave = Autosave::default();
        self.error = None;
        self.mode = Mode::Edit;
        self.active.store(note.id, Ordering::Relaxed);
        self.send(WriteReq::SetActive { note_id: note.id });
    }

    // ---- switching notes: always save a dirty buffer first ----

    pub fn open_note(&mut self, id: i64, _now: Instant) {
        self.switch(Target::Note(id));
    }

    pub fn import_recovered(&mut self, path: PathBuf, _now: Instant) {
        self.switch(Target::Import(path));
    }

    fn switch(&mut self, target: Target) {
        self.mode = Mode::Edit;
        if self.dirty() || self.in_flight.is_some() {
            self.pending_switch = Some(target);
            self.send_save();
        } else {
            self.do_switch(target);
        }
    }

    fn do_switch(&mut self, target: Target) {
        match target {
            Target::Note(id) => match db::load_note(&self.reader, id) {
                Ok(Some(n)) => self.load_into(n),
                Ok(None) => self.message = Some("That note no longer exists".into()),
                Err(e) => self.message = Some(e.to_string()),
            },
            Target::New => {
                self.pending_insert = Some(PendingInsert::New);
                self.send(WriteReq::Insert { body: String::new() });
            }
            Target::Import(path) => match std::fs::read_to_string(&path) {
                Ok(body) => {
                    self.pending_insert = Some(PendingInsert::Import(path, body.clone()));
                    self.send(WriteReq::Insert { body });
                }
                Err(e) => self.message = Some(format!("{}: {e}", path.display())),
            },
        }
    }

    // ---- quitting ----

    fn quit(&mut self) {
        self.quitting = true;
        if self.dirty() || self.in_flight.is_some() {
            self.send_save();
        } else {
            self.finish_exit();
        }
    }

    pub fn on_signal(&mut self) {
        if self.pending_insert.is_some() {
            self.deferred.push(Deferred::Signal);
            return;
        }
        self.forced = true;
        self.quit();
    }

    fn finish_exit(&mut self) {
        self.send(WriteReq::DeleteEmpty);
        self.exit = true;
    }

    /// A signal can't wait for the user: keep the text in a recovery file, then go.
    fn forced_exit(&mut self) {
        if self.dirty() {
            if let Err(e) = recovery::export(&self.recovery_dir, self.buf.text()) {
                let why = self.error.clone().unwrap_or_else(|| "conflict".into());
                self.fatal = Some(format!("unsaved text lost: save failed ({why}), recovery export failed ({e})"));
            }
        }
        self.exit = true;
    }

    fn export(&mut self) {
        self.message = Some(match recovery::export(&self.recovery_dir, self.buf.text()) {
            Ok(p) => format!("Exported to {}", p.display()),
            Err(e) => format!("Export failed: {e}"),
        });
    }

    // ---- events ----

    pub fn on_reply(&mut self, r: WriteReply, now: Instant) {
        self.handle_reply(r, now);
        if self.pending_insert.is_none() && !self.deferred.is_empty() {
            for d in std::mem::take(&mut self.deferred) {
                match d {
                    Deferred::Key(k) => self.on_key(k, now),
                    Deferred::Paste(s) => self.on_paste(&s, now),
                    Deferred::Signal => self.on_signal(),
                }
            }
        }
    }

    fn handle_reply(&mut self, r: WriteReply, now: Instant) {
        match r {
            WriteReply::Saved { note_id, new_rev, gen } if note_id == self.note_id => {
                self.in_flight = None;
                self.base_rev = new_rev;
                self.saved_gen = gen;
                self.error = None;
                self.autosave.on_saved(now, self.dirty());
                if self.quitting {
                    if self.dirty() {
                        self.send_save();
                    } else {
                        self.finish_exit();
                    }
                } else if self.pending_switch.is_some() {
                    if self.dirty() {
                        self.send_save();
                    } else if let Some(t) = self.pending_switch.take() {
                        self.do_switch(t);
                    }
                }
            }
            WriteReply::Conflict { note_id } if note_id == self.note_id => {
                self.in_flight = None;
                self.pending_switch = None;
                self.mode = Mode::Conflict;
                if self.forced {
                    self.forced_exit();
                } else {
                    self.quitting = false; // resolve first, then Esc again
                }
            }
            WriteReply::Error { note_id, msg } if note_id.is_none() || note_id == Some(self.note_id) => {
                if note_id.is_none() && self.pending_insert.take().is_some() {
                    self.message = Some(msg);
                    return;
                }
                self.in_flight = None;
                self.pending_switch = None;
                self.error = Some(msg);
                self.autosave.on_failed(now);
                if self.forced {
                    self.forced_exit();
                } else if self.quitting {
                    self.mode = Mode::QuitError;
                }
            }
            WriteReply::Inserted { note_id } => match self.pending_insert.take() {
                Some(PendingInsert::SaveAsNew { gen }) => {
                    self.note_id = note_id;
                    self.base_rev = 0;
                    self.saved_gen = gen;
                    self.error = None;
                    self.mode = Mode::Edit;
                    self.autosave = Autosave::default();
                    if self.dirty() {
                        self.autosave.on_edit(now);
                    }
                    self.active.store(note_id, Ordering::Relaxed);
                    self.send(WriteReq::SetActive { note_id });
                    self.message = Some("Saved as a new note".into());
                }
                Some(PendingInsert::New) => {
                    self.load_into(Note { id: note_id, body: String::new(), rev: 0, cursor: 0 })
                }
                Some(PendingInsert::Import(path, body)) => {
                    self.load_into(Note { id: note_id, body, rev: 0, cursor: 0 });
                    if let Err(e) = std::fs::remove_file(&path) {
                        self.message = Some(format!("Imported, but could not remove {}: {e}", path.display()));
                    }
                }
                None => {}
            },
            _ => {} // reply for a note we already left
        }
    }

    pub fn tick(&mut self, now: Instant) {
        if !matches!(self.mode, Mode::Conflict) && self.autosave.due(now) {
            self.send_save();
        }
        if now.duration_since(self.last_poll) >= POLL && matches!(self.mode, Mode::Edit) && self.in_flight.is_none() {
            self.last_poll = now;
            self.poll_external();
        }
    }

    /// Another window committed: reload our note only if we have nothing unsaved.
    fn poll_external(&mut self) {
        let Ok(dv) = db::data_version(&self.reader) else { return };
        if dv == self.last_data_version {
            return;
        }
        self.last_data_version = dv;
        if let Ok(Some(n)) = db::load_note(&self.reader, self.note_id) {
            if n.rev != self.base_rev && !self.dirty() {
                self.buf = Buffer::new(n.body, self.buf.cursor());
                self.base_rev = n.rev;
                self.saved_gen = 0;
            }
        }
    }

    pub fn on_paste(&mut self, s: &str, now: Instant) {
        if self.pending_insert.is_some() {
            // typing into the note being created is kept; a repeated Conflict command is not
            if matches!(self.mode, Mode::Edit) {
                self.deferred.push(Deferred::Paste(s.to_string()));
            }
            return;
        }
        self.message = None;
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        match &mut self.mode {
            Mode::Edit => self.insert(&s, now),
            Mode::Search { query, .. } => {
                let _ = query.insert_str(&s.replace('\n', " "));
                self.refresh_search();
            }
            Mode::CategoryPrompt { input } => {
                let _ = input.insert_str(&s.replace('\n', " "));
            }
            _ => {}
        }
    }

    pub fn on_key(&mut self, k: KeyEvent, now: Instant) {
        if self.pending_insert.is_some() {
            // typing into the note being created is kept; a repeated Conflict command is not
            if matches!(self.mode, Mode::Edit) {
                self.deferred.push(Deferred::Key(k));
            }
            return;
        }
        self.message = None;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match self.mode {
            Mode::Edit => self.edit_key(k, ctrl, now),
            Mode::Conflict => match k.code {
                KeyCode::Char('r') => self.conflict_reload(),
                KeyCode::Char('o') => self.conflict_overwrite(),
                KeyCode::Char('n') => {
                    self.pending_insert = Some(PendingInsert::SaveAsNew { gen: self.buf.gen() });
                    let body = self.buf.text().to_string();
                    self.send(WriteReq::Insert { body });
                }
                _ => {}
            },
            Mode::QuitError => match k.code {
                KeyCode::Char('e') => self.export(),
                KeyCode::Char('Q') => self.mode = Mode::QuitConfirm,
                KeyCode::Esc => {
                    self.quitting = false;
                    self.mode = Mode::Edit;
                }
                _ => {}
            },
            Mode::QuitConfirm => match k.code {
                KeyCode::Char('Q') => self.exit = true,
                KeyCode::Esc => self.mode = Mode::QuitError,
                _ => {}
            },
            Mode::List { .. } => self.list_key(k, now),
            Mode::Search { .. } => self.search_key(k, now),
            Mode::CategoryPrompt { .. } => self.category_key(k),
        }
    }

    fn edit_key(&mut self, k: KeyEvent, ctrl: bool, now: Instant) {
        match k.code {
            KeyCode::Esc => self.quit(),
            KeyCode::Char('l') if ctrl => {
                self.list = db::list_notes(&self.reader).unwrap_or_default();
                self.recovered = recovery::list(&self.recovery_dir).unwrap_or_default();
                self.mode = Mode::List { sel: 0 };
            }
            KeyCode::Char('f') if ctrl => {
                self.results.clear();
                self.mode = Mode::Search { query: Buffer::new(String::new(), 0), sel: 0 };
            }
            KeyCode::Char('n') if ctrl => self.switch(Target::New),
            KeyCode::Char('k') if ctrl => {
                self.mode = Mode::CategoryPrompt { input: Buffer::new(String::new(), 0) }
            }
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => {
                self.insert(c.encode_utf8(&mut [0; 4]), now)
            }
            KeyCode::Enter => self.insert("\n", now),
            KeyCode::Tab => self.insert("\t", now),
            KeyCode::Backspace => self.edit(now, Buffer::backspace),
            KeyCode::Delete => self.edit(now, Buffer::delete),
            KeyCode::Left => self.buf.left(),
            KeyCode::Right => self.buf.right(),
            KeyCode::Up => self.buf.up(),
            KeyCode::Down => self.buf.down(),
            KeyCode::Home => self.buf.home(),
            KeyCode::End => self.buf.end(),
            _ => {}
        }
    }

    fn conflict_reload(&mut self) {
        if let Ok(Some(n)) = db::load_note(&self.reader, self.note_id) {
            self.buf = Buffer::new(n.body, self.buf.cursor());
            self.base_rev = n.rev;
            self.saved_gen = 0;
            self.autosave = Autosave::default();
            self.error = None;
            self.mode = Mode::Edit;
        }
    }

    fn conflict_overwrite(&mut self) {
        if let Ok(Some(n)) = db::load_note(&self.reader, self.note_id) {
            self.base_rev = n.rev;
            self.mode = Mode::Edit;
            self.send_save();
        }
    }

    fn list_len(&self) -> usize {
        self.recovered.len() + self.list.len()
    }

    fn list_key(&mut self, k: KeyEvent, now: Instant) {
        let Mode::List { sel } = self.mode else { return };
        match k.code {
            KeyCode::Esc => self.mode = Mode::Edit,
            KeyCode::Up => self.mode = Mode::List { sel: sel.saturating_sub(1) },
            KeyCode::Down => self.mode = Mode::List { sel: (sel + 1).min(self.list_len().saturating_sub(1)) },
            KeyCode::Enter if sel < self.recovered.len() => {
                let path = self.recovered[sel].clone();
                self.import_recovered(path, now);
            }
            KeyCode::Enter if sel < self.list_len() => {
                let id = self.list[sel - self.recovered.len()].id;
                self.open_note(id, now);
            }
            _ => {}
        }
    }

    fn refresh_search(&mut self) {
        if let Mode::Search { query, sel } = &mut self.mode {
            self.results = db::search(&self.reader, query.text()).unwrap_or_default();
            *sel = 0;
        }
    }

    fn search_key(&mut self, k: KeyEvent, now: Instant) {
        let Mode::Search { query, sel } = &mut self.mode else { return };
        let changed = match k.code {
            KeyCode::Esc => {
                self.mode = Mode::Edit;
                return;
            }
            KeyCode::Enter => {
                if let Some(&(id, _)) = self.results.get(*sel) {
                    self.open_note(id, now);
                }
                return;
            }
            KeyCode::Up => {
                *sel = sel.saturating_sub(1);
                false
            }
            KeyCode::Down => {
                *sel = (*sel + 1).min(self.results.len().saturating_sub(1));
                false
            }
            _ => line_input(query, k),
        };
        if changed {
            self.refresh_search();
        }
    }

    fn category_key(&mut self, k: KeyEvent) {
        let Mode::CategoryPrompt { input } = &mut self.mode else { return };
        match k.code {
            KeyCode::Esc => self.mode = Mode::Edit,
            KeyCode::Enter => {
                let category = Some(input.text().trim().to_string()).filter(|c| !c.is_empty());
                let note_id = self.note_id;
                self.mode = Mode::Edit;
                self.send(WriteReq::SetCategory { note_id, category });
            }
            _ => {
                line_input(input, k);
            }
        }
    }
}

/// Single-line text field editing; returns whether the text changed.
fn line_input(b: &mut Buffer, k: KeyEvent) -> bool {
    let before = b.gen();
    match k.code {
        KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
            let _ = b.insert_str(c.encode_utf8(&mut [0; 4]));
        }
        KeyCode::Backspace => b.backspace(),
        KeyCode::Delete => b.delete(),
        KeyCode::Left => b.left(),
        KeyCode::Right => b.right(),
        KeyCode::Home => b.home(),
        KeyCode::End => b.end(),
        _ => {}
    }
    b.gen() != before
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::mpsc::{self, Receiver};
    use std::time::Duration;

    pub(crate) fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }
    fn key(c: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(c, m)
    }
    fn esc() -> KeyEvent {
        key(KeyCode::Esc, KeyModifiers::NONE)
    }
    fn enter() -> KeyEvent {
        key(KeyCode::Enter, KeyModifiers::NONE)
    }
    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        key(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    pub(crate) fn app_p() -> (tempfile::TempDir, PathBuf, App, Receiver<WriteReq>) {
        let (d, p, c) = db::test_db();
        let id = db::insert_note(&c, "", 0).unwrap();
        let note = db::load_note(&c, id).unwrap().unwrap();
        let (tx, rx) = mpsc::channel();
        let a = App::new(db::open_reader(&p).unwrap(), tx, note, d.path().join("recovery"), Arc::new(AtomicI64::new(0)));
        (d, p, a, rx)
    }
    fn app() -> (tempfile::TempDir, App, Receiver<WriteReq>) {
        let (d, _p, a, rx) = app_p();
        (d, a, rx)
    }

    // ---- Task 5 ----
    #[test]
    fn autosave_cycle_and_status() {
        let (_d, mut a, rx) = app();
        let t = Instant::now();
        a.on_paste("hi", t);
        assert_eq!(a.status(), Status::Unsaved);
        a.tick(t + ms(1000));
        let WriteReq::Save { expected_rev: 0, gen, .. } = rx.try_recv().unwrap() else { panic!() };
        assert_eq!(a.status(), Status::Saving);
        a.on_reply(WriteReply::Saved { note_id: a.note_id, new_rev: 1, gen }, t + ms(1010));
        assert_eq!(a.status(), Status::Saved);
        a.on_paste("!", t + ms(1020));
        assert_eq!(a.status(), Status::Unsaved);
    }

    #[test]
    fn paste_over_limit_rejected_with_message() {
        let (_d, mut a, _rx) = app();
        a.on_paste(&"a".repeat(512_001), Instant::now());
        assert_eq!(a.buf.text(), "");
        assert_eq!(a.message.as_deref(), Some("Note is limited to 512,000 bytes; edit rejected"));
    }

    #[test]
    fn paste_normalizes_cr() {
        let (_d, mut a, _rx) = app();
        a.on_paste("a\r\nb\rc", Instant::now());
        assert_eq!(a.buf.text(), "a\nb\nc");
    }

    #[test]
    fn error_keeps_buffer_and_retries() {
        let (_d, mut a, rx) = app();
        let t = Instant::now();
        a.on_paste("x", t);
        a.tick(t + ms(1000));
        rx.try_recv().unwrap();
        a.on_reply(WriteReply::Error { note_id: Some(a.note_id), msg: "disk full".into() }, t + ms(1100));
        assert_eq!((a.buf.text(), a.status()), ("x", Status::Error("disk full".into())));
        a.tick(t + ms(2100));
        assert!(matches!(rx.try_recv(), Ok(WriteReq::Save { .. })));
    }

    #[test]
    fn quit_waits_for_current_gen() {
        let (_d, mut a, rx) = app();
        let t = Instant::now();
        a.on_paste("a", t);
        a.tick(t + ms(1000));
        let WriteReq::Save { gen: g1, .. } = rx.try_recv().unwrap() else { panic!() };
        a.on_paste("b", t + ms(1001));
        a.on_key(esc(), t + ms(1002));
        a.on_reply(WriteReply::Saved { note_id: a.note_id, new_rev: 1, gen: g1 }, t + ms(1003));
        assert!(!a.should_exit());
        let WriteReq::Save { expected_rev: 1, gen: g2, .. } = rx.try_recv().unwrap() else { panic!() };
        a.on_reply(WriteReply::Saved { note_id: a.note_id, new_rev: 2, gen: g2 }, t + ms(1004));
        assert!(a.should_exit());
    }

    #[test]
    fn quit_on_error_offers_export_then_double_q() {
        let (_d, mut a, rx) = app();
        let t = Instant::now();
        a.on_paste("keep me", t);
        a.on_key(esc(), t);
        rx.try_recv().unwrap();
        a.on_reply(WriteReply::Error { note_id: Some(a.note_id), msg: "ro".into() }, t);
        assert!(matches!(a.mode, Mode::QuitError) && !a.should_exit());
        a.on_key(ch('e'), t);
        assert_eq!(recovery::list(&a.recovery_dir).unwrap().len(), 1);
        a.on_key(ch('Q'), t);
        assert!(!a.should_exit());
        a.on_key(ch('Q'), t);
        assert!(a.should_exit());
    }

    #[test]
    fn signal_exits_even_on_error() {
        let (_d, mut a, rx) = app();
        let t = Instant::now();
        a.on_paste("x", t);
        a.on_signal();
        rx.try_recv().unwrap();
        a.on_reply(WriteReply::Error { note_id: Some(a.note_id), msg: "e".into() }, t);
        assert!(a.should_exit());
        assert_eq!(recovery::list(&a.recovery_dir).unwrap().len(), 1); // ruling: forced exit exports
    }

    // ---- final review fixes ----
    #[test]
    fn keys_during_pending_new_note_are_kept() {
        let (_d, mut a, rx) = app();
        let t = Instant::now();
        a.on_key(ctrl('n'), t);
        assert!(matches!(rx.try_recv().unwrap(), WriteReq::Insert { .. }));
        a.on_paste("abc", t);
        a.on_reply(WriteReply::Inserted { note_id: 7 }, t);
        assert_eq!((a.note_id, a.buf.text(), a.status()), (7, "abc", Status::Unsaved));
    }

    #[test]
    fn signal_during_save_as_new_still_exits() {
        let (_d, _p, mut a, rx) = into_conflict();
        let t = Instant::now();
        a.on_key(ch('n'), t);
        a.on_key(ch('n'), t); // second press must not send a second insert
        a.on_signal();
        assert!(matches!(rx.try_recv().unwrap(), WriteReq::Insert { .. }));
        assert!(rx.try_recv().is_err());
        a.on_reply(WriteReply::Inserted { note_id: 99 }, t);
        assert!(a.should_exit());
    }

    #[test]
    fn forced_exit_reports_failed_export() {
        let (_d, mut a, rx) = app();
        a.recovery_dir = PathBuf::from("/dev/null/recovery"); // cannot be created
        let t = Instant::now();
        a.on_paste("precious", t);
        a.on_signal();
        rx.try_recv().unwrap();
        a.on_reply(WriteReply::Error { note_id: Some(a.note_id), msg: "disk full".into() }, t);
        assert!(a.should_exit());
        assert!(a.fatal.as_deref().unwrap_or("").contains("disk full"));
    }

    #[test]
    fn conflict_during_quit_cancels_quit() {
        let (_d, p, mut a, rx) = app_p();
        let t = Instant::now();
        db::save_note(&db::open(&p).unwrap(), a.note_id, 0, "theirs", 0, 1).unwrap();
        a.on_paste("mine", t);
        a.on_key(esc(), t);
        rx.try_recv().unwrap();
        a.on_reply(WriteReply::Conflict { note_id: a.note_id }, t);
        a.on_key(ch('r'), t);
        a.on_paste("!", t);
        a.tick(t + ms(1000));
        let WriteReq::Save { gen, .. } = rx.try_recv().unwrap() else { panic!() };
        a.on_reply(WriteReply::Saved { note_id: a.note_id, new_rev: 2, gen }, t + ms(1010));
        assert!(!a.should_exit());
    }

    // ---- Task 6 ----
    fn into_conflict() -> (tempfile::TempDir, PathBuf, App, Receiver<WriteReq>) {
        let (d, p, mut a, rx) = app_p();
        let t = Instant::now();
        db::save_note(&db::open(&p).unwrap(), a.note_id, 0, "theirs", 0, 1).unwrap();
        a.on_paste("mine", t);
        a.tick(t + ms(1000));
        rx.try_recv().unwrap();
        a.on_reply(WriteReply::Conflict { note_id: a.note_id }, t);
        (d, p, a, rx)
    }

    #[test]
    fn conflict_reload_takes_db_version() {
        let (_d, _p, mut a, _rx) = into_conflict();
        assert_eq!((a.status(), a.buf.text()), (Status::Conflict, "mine"));
        a.on_key(ch('r'), Instant::now());
        assert_eq!((a.buf.text(), a.status()), ("theirs", Status::Saved));
    }

    #[test]
    fn conflict_overwrite_uses_current_rev() {
        let (_d, _p, mut a, rx) = into_conflict();
        a.on_key(ch('o'), Instant::now());
        assert!(matches!(rx.try_recv().unwrap(), WriteReq::Save { expected_rev: 1, .. }));
    }

    #[test]
    fn conflict_save_as_new_keeps_text() {
        let (_d, _p, mut a, rx) = into_conflict();
        let t = Instant::now();
        a.on_key(ch('n'), t);
        assert!(matches!(rx.try_recv().unwrap(), WriteReq::Insert { ref body } if body == "mine"));
        a.on_reply(WriteReply::Inserted { note_id: 99 }, t);
        assert_eq!((a.note_id, a.buf.text(), a.status()), (99, "mine", Status::Saved));
    }

    #[test]
    fn clean_buffer_reloads_on_external_change_dirty_does_not() {
        let (_d, p, mut a, _rx) = app_p();
        let t = Instant::now();
        let other = db::open(&p).unwrap();
        db::save_note(&other, a.note_id, 0, "ext", 0, 1).unwrap();
        a.tick(t + ms(500));
        assert_eq!(a.buf.text(), "ext");
        a.on_paste("!", t + ms(600));
        let mine = a.buf.text().to_string();
        db::save_note(&other, a.note_id, 1, "ext2", 0, 2).unwrap();
        a.tick(t + ms(1100));
        assert_eq!(a.buf.text(), mine);
    }

    #[test]
    fn poll_during_own_save_does_not_reload() {
        let (_d, p, mut a, rx) = app_p();
        let t = Instant::now();
        a.on_paste("mine", t);
        a.tick(t + ms(1000));
        let WriteReq::Save { gen, .. } = rx.try_recv().unwrap() else { panic!() };
        db::save_note(&db::open(&p).unwrap(), a.note_id, 0, "mine", 4, 2).unwrap(); // our own save committing
        a.tick(t + ms(1500));
        assert_eq!(a.buf.text(), "mine");
        a.on_reply(WriteReply::Saved { note_id: a.note_id, new_rev: 1, gen }, t + ms(1600));
        a.tick(t + ms(2000));
        assert_eq!((a.buf.text(), a.status()), ("mine", Status::Saved));
    }

    // ---- Task 7 ----
    #[test]
    fn switch_with_dirty_buffer_saves_first() {
        let (_d, p, mut a, rx) = app_p();
        let t = Instant::now();
        let other = db::insert_note(&db::open(&p).unwrap(), "other", 1).unwrap();
        a.on_paste("dirty", t);
        a.open_note(other, t);
        let WriteReq::Save { gen, .. } = rx.try_recv().unwrap() else { panic!() };
        assert_ne!(a.note_id, other);
        a.on_reply(WriteReply::Saved { note_id: a.note_id, new_rev: 1, gen }, t);
        assert_eq!((a.note_id, a.buf.text()), (other, "other"));
    }

    #[test]
    fn switch_cancelled_on_conflict() {
        let (_d, p, mut a, rx) = app_p();
        let t = Instant::now();
        let other = db::insert_note(&db::open(&p).unwrap(), "other", 1).unwrap();
        let first = a.note_id;
        a.on_paste("dirty", t);
        a.open_note(other, t);
        rx.try_recv().unwrap();
        a.on_reply(WriteReply::Conflict { note_id: first }, t);
        assert_eq!((a.note_id, a.buf.text(), a.status()), (first, "dirty", Status::Conflict));
    }

    #[test]
    fn category_prompt_sends_set_category() {
        let (_d, mut a, rx) = app();
        let t = Instant::now();
        a.on_key(ctrl('k'), t);
        for c in "work".chars() {
            a.on_key(ch(c), t);
        }
        a.on_key(enter(), t);
        assert!(matches!(rx.try_recv().unwrap(), WriteReq::SetCategory { category: Some(ref c), .. } if c == "work"));
    }

    #[test]
    fn import_recovered_removes_file_after_insert() {
        let (_d, mut a, rx) = app();
        let t = Instant::now();
        let f = recovery::export(&a.recovery_dir, "rescued").unwrap();
        a.import_recovered(f.clone(), t);
        assert!(matches!(rx.try_recv().unwrap(), WriteReq::Insert { ref body } if body == "rescued"));
        assert!(f.exists());
        a.on_reply(WriteReply::Inserted { note_id: 5 }, t);
        assert!(!f.exists() && a.note_id == 5);
    }
}
