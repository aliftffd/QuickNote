use crate::db;
use rusqlite::Connection;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread::JoinHandle;

pub enum WriteReq {
    Save { note_id: i64, expected_rev: i64, body: String, cursor: usize, gen: u64 },
    Insert { body: String },
    SetActive { note_id: i64 },
    SetCategory { note_id: i64, category: Option<String> },
    DeleteEmpty,
}

#[derive(Debug, PartialEq)]
pub enum WriteReply {
    Saved { note_id: i64, new_rev: i64, gen: u64 },
    Conflict { note_id: i64 },
    Inserted { note_id: i64 },
    Error { note_id: Option<i64>, msg: String },
}

/// The only writer for user edits. Exits when every `Sender<WriteReq>` is dropped,
/// after draining what was already queued.
pub fn spawn(conn: Connection, ai_wake: SyncSender<()>) -> (Sender<WriteReq>, Receiver<WriteReply>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel::<WriteReq>();
    let (reply_tx, reply_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        for req in rx {
            let reply = handle(&conn, req, &ai_wake);
            if let Some(r) = reply {
                let _ = reply_tx.send(r);
            }
        }
    });
    (tx, reply_rx, handle)
}

fn handle(c: &Connection, req: WriteReq, ai_wake: &SyncSender<()>) -> Option<WriteReply> {
    let err = |note_id, e: rusqlite::Error| WriteReply::Error { note_id, msg: e.to_string() };
    match req {
        WriteReq::Save { note_id, expected_rev, body, cursor, gen } => {
            Some(match db::save_note(c, note_id, expected_rev, &body, cursor, db::now_ms()) {
                Ok(Some(new_rev)) => {
                    let _ = ai_wake.try_send(()); // capacity 1: wake-ups coalesce
                    WriteReply::Saved { note_id, new_rev, gen }
                }
                Ok(None) => WriteReply::Conflict { note_id },
                Err(e) => err(Some(note_id), e),
            })
        }
        WriteReq::Insert { body } => Some(match db::insert_note(c, &body, db::now_ms()) {
            Ok(note_id) => {
                let _ = ai_wake.try_send(());
                WriteReply::Inserted { note_id }
            }
            Err(e) => err(None, e),
        }),
        WriteReq::SetActive { note_id } => db::kv_set(c, "active_note_id", &note_id.to_string())
            .err()
            .map(|e| err(Some(note_id), e)),
        WriteReq::SetCategory { note_id, category } => db::set_manual_category(c, note_id, category.as_deref())
            .err()
            .map(|e| err(Some(note_id), e)),
        WriteReq::DeleteEmpty => db::delete_empty_notes(c).err().map(|e| err(None, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;

    fn setup() -> (tempfile::TempDir, i64, Sender<WriteReq>, Receiver<WriteReply>, Receiver<()>, JoinHandle<()>) {
        let (d, _p, c) = crate::db::test_db();
        let id = crate::db::insert_note(&c, "a", 1).unwrap();
        let (wtx, wrx) = std::sync::mpsc::sync_channel(1);
        let (tx, rx, h) = spawn(c, wtx);
        (d, id, tx, rx, wrx, h)
    }

    #[test]
    fn save_ok_then_conflict() {
        let (_d, id, tx, rx, wake, _h) = setup();
        let save = |rev| WriteReq::Save { note_id: id, expected_rev: rev, body: "b".into(), cursor: 1, gen: 7 };
        tx.send(save(0)).unwrap();
        assert_eq!(rx.recv().unwrap(), WriteReply::Saved { note_id: id, new_rev: 1, gen: 7 });
        assert!(wake.try_recv().is_ok());
        tx.send(save(0)).unwrap();
        assert_eq!(rx.recv().unwrap(), WriteReply::Conflict { note_id: id });
    }

    #[test]
    fn sqlite_error_becomes_error_reply() {
        let (_d, id, tx, rx, _w, _h) = setup();
        tx.send(WriteReq::Save { note_id: id, expected_rev: 0, body: "a".repeat(512_001), cursor: 0, gen: 1 })
            .unwrap();
        assert!(matches!(rx.recv().unwrap(), WriteReply::Error { note_id: Some(n), .. } if n == id));
    }

    #[test]
    fn exits_when_sender_dropped() {
        let (_d, _id, tx, _rx, _w, h) = setup();
        drop(tx);
        h.join().unwrap();
    }
}
