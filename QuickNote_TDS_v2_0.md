# QuickNote — Technical Design Specification v2.0

Status: proposed replacement for v1.4. Not yet implemented or tested.
Revision 2026-10-27: the categorizer is now a `TextMlp` from `rust_ann` (pure Rust, trained by `quicknote train`) instead of ONNX (§6, §6a).

## 0. What changed from v1.4 and why

v1.4 put a daemon between the editor and the database. Most of its complexity came from that one choice: ownership tokens, Freeze/FreezeAck, stream/seq deduplication, DurableAck, retry buffers, mirror resync, IPC framing and chunking, and the Esc-timeout recovery path. Each of these exists only to handle the case where the daemon and a client fail separately.

v2.0 removes the daemon. Each TUI window is one process that opens SQLite directly. SQLite in WAL mode already handles several processes sharing one database file. If two windows save over each other, a revision check on save catches it, so ownership tokens are not needed.

The durability guarantee stays the same: a change is safe once its SQLite transaction commits. The UI never shows `Saved` before that commit. About 1–5 s of typing can be lost if the process crashes. v1.4 had roughly the same loss window, because its retry buffer lived in client RAM.

## 1. Goals and guarantees

QuickNote is a TUI note app backed by SQLite. Its core feature is AI that sorts every note into a category. If the AI is unavailable, manual categories are the fallback: notes can still be written and categorized by hand.

- **Latency:** p95 time from a keystroke arriving to the TUI frame being rendered is under 16 ms, including while inference or a save is running. Measure it on a named terminal, machine and workload.
- **The editor never waits** for disk I/O or inference. Both run on their own threads.
- **Durability boundary:** a transaction committed with `journal_mode=WAL` and `synchronous=FULL`. This assumes the filesystem and the disk honor fsync.
- **Loss window:** if the process dies, edits since the last successful autosave are lost. That's at most about 5 s of typing (see §4).
- **No silent loss:** a failed save, a conflict or a rejected edit never discards text and never shows `Saved`.

## 2. Architecture

One process per window, with three threads. Threads talk over `std::sync::mpsc` channels. There is no async runtime.

| Thread | Owns | Does |
|---|---|---|
| UI | The terminal, the editor buffer and a read-only SQLite connection | Input, rendering, note list, search, status indicator |
| Save | A writer SQLite connection | Runs save transactions and replies with the result |
| AI | A writer SQLite connection and the loaded `TextMlp` model | Works through the categorization backlog (§6) |

Stack: Rust, `ratatui` + `crossterm`, `rusqlite` (bundled, with FTS5), and `rust_ann` (path dependency `CNN-ANN_rust/`, pure Rust, no native libraries) for the model.

Every connection sets: `journal_mode=WAL`, `synchronous=FULL`, `busy_timeout=5000`, `foreign_keys=ON`. Opening the database checks that `journal_mode` actually returned `wal`. If it didn't, the app stops at startup with an error.

## 3. Schema

```sql
CREATE TABLE notes (
  id              INTEGER PRIMARY KEY,
  body            TEXT    NOT NULL DEFAULT '' CHECK (length(CAST(body AS BLOB)) <= 512000),
  rev             INTEGER NOT NULL DEFAULT 0,       -- +1 per successful save
  cursor          INTEGER NOT NULL DEFAULT 0,       -- UTF-8 byte offset
  created_at      INTEGER NOT NULL,                 -- unix ms
  updated_at      INTEGER NOT NULL,
  category_manual TEXT,                             -- set by user; always wins
  category_ai     TEXT,                             -- last model prediction
  ai_rev          INTEGER,                          -- rev the prediction was made for
  ai_model        TEXT                              -- model_version that made it
);

CREATE VIRTUAL TABLE notes_fts USING fts5(body, content='notes', content_rowid='id');
-- AFTER INSERT / UPDATE OF body / DELETE triggers keep notes_fts in sync
-- inside the same transaction.

CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);  -- active_note_id
```

Where a note is displayed:

- `category_manual`, if the user has set one.
- Otherwise `category_ai`. If `ai_rev < rev`, the category is shown with a "stale" marker, because it was predicted for an older version of the text.
- Otherwise the note goes in the `Uncategorized` group.

Upgrade the schema with `PRAGMA user_version` and a list of SQL migrations, applied in order at startup.

## 4. Editing and autosave

The editor buffer lives on the UI thread. Each edit increments a local `gen` counter. Nothing is sent per keystroke.

**When to save:** the buffer is dirty and either (a) it has been idle for 1 s, or (b) 5 s have passed since the last successful save. If the buffer becomes dirty after a long idle period, the 5 s timer starts at that first edit, not from the last save. At most one save per note is in flight at a time. If the buffer changes during a save, the next save is scheduled after the reply arrives.

**Save request:** `Save { note_id, expected_rev, body, cursor, gen }` → save thread:

```sql
UPDATE notes SET body=?, rev=rev+1, cursor=?, updated_at=?
WHERE id=? AND rev=?expected_rev;
```

**Reply:**
- 1 row changed → `Saved { note_id, new_rev, gen }`. The UI sets `base_rev = new_rev`. The indicator shows `Saved` only if `gen` still equals the current gen.
- 0 rows changed → `Conflict { note_id }`. Another window saved first. See §5.
- SQLite error → `Error { note_id, msg }`. Keep the buffer and retry on the next trigger. The indicator shows `Error` with the message.

**Size limit:** 512,000 bytes of UTF-8 per note. An edit or paste that would go over the limit is rejected at input time with a message, and the buffer is left unchanged. The `CHECK` constraint in the schema is the second line of defense.

## 5. Multiple windows

There is no ownership, focus tracking or mirroring protocol. Windows coordinate only through the database.

- **External changes:** while the UI is idle, it checks `PRAGMA data_version` every 500 ms. If the value changed, the UI refreshes the note list. If the open note's `rev` moved and the local buffer is **clean**, the UI reloads the note and clamps the cursor.
- **Conflict:** if the local buffer is **dirty** and the database moved, the next save returns `Conflict`. The UI keeps the buffer, shows `Conflict`, and offers three options:
  - `r` reload: discard local changes and take the database version.
  - `o` overwrite: save again with `expected_rev` set to the current rev.
  - `n` save as new note: insert the buffer as a new note. Nothing is lost.

  The choice is never made automatically.

## 6. AI categorization

There is one AI thread and one inference at a time. The model is a `rust_ann::layers::TextMlp`: hashed character 3–5-grams plus words → `Embedding(65536, 32)` → mean → `Linear(32)` → ReLU → `Linear(n_categories)` → softmax. It runs on that one thread.

**The model file** is a single `<data dir>/model/model.bin`: `[u64 version][u32 length][labels, one per line; line i is output i][weights from Tape::save_params]`. The version (unix ms) is the `model_version`. Keeping everything in one file means a reader can never pair new weights with old labels.

The architecture constants (65536, 32, 32) are fixed in code. A model trained with different constants fails `load_params` and shows as paused.

**Categories** are the user's own manual categories. The model can only predict categories that existed when it was trained.

**Confidence cutoff:** if the top probability is below `QUICKNOTE_MIN_CONFIDENCE` (default `0.7`; with only 2 categories the top probability is always at least 0.5, so a lower cutoff never fires), the note gets no AI category and is shown in `Uncategorized`. It still counts as processed for that `rev`.

The backlog is the `notes` table itself. No job queue is stored, so restarts and model changes need no extra handling:

```sql
SELECT id, body, rev FROM notes
WHERE category_manual IS NULL
  AND (ai_rev IS NULL OR ai_rev < rev OR ai_model IS NOT ?current_model)
  AND updated_at <= ?now_minus_4s          -- skip notes being actively typed
ORDER BY (id = ?active_note_id) DESC, updated_at DESC
LIMIT 1;
```

Writing the result:

```sql
UPDATE notes SET category_ai=?, ai_rev=?rev, ai_model=?
WHERE id=? AND rev=?rev AND category_manual IS NULL;
```

What this gives you without extra code:

- **Stale results** are dropped: 0 rows are updated and the note is picked up again later.
- **Manual categories** are never overwritten.
- **Coalescing per note:** only the latest saved `rev` is ever selected.
- **No starvation:** a note leaves the backlog as soon as it is processed, and the backlog only grows as fast as a person edits notes.
- **Restarts and a new `model_version`** re-select every note that needs processing.

The AI thread waits on a channel for wake-ups. The save thread sends one after each commit. If no wake-up arrives, the thread checks again every 30 s. If the backlog is empty, the thread sleeps.

**Model unavailable or failing:** the UI shows `auto-categorize: paused (<reason>)`. When there is no model directory yet, the reason is `no model yet, run quicknote train`. The thread tries to load the model again every 60 s. Notes stay in `Uncategorized` or keep their stale marker, and manual categorization keeps working.

**New model:** every time the AI thread wakes (§6, every 30 s at most), and before each note it classifies, it re-reads the version header of `model.bin`. If the contents changed, it reloads the model. The backlog query then picks up every note whose `ai_model` differs from the new version.

**Panics:** inference runs inside `catch_unwind`. A panic counts as that note's classify error (the note is marked and skipped). It does not kill the AI thread.

## 6a. Training (`quicknote train`)

`quicknote train` is a one-shot command. It is safe to run while windows are open.

1. Read every note where `category_manual IS NOT NULL`.
2. Keep only categories with **at least 5** notes. If fewer than **2** categories remain, print which categories need more notes and exit with status 1. No model files are written.
3. Split each category 80/20 into train and test sets, with at least 1 test note per category. Shuffle with a fixed seed so runs are repeatable.
4. Train with Adam (lr 0.01), batch 8, 30 epochs.
5. Print the train and test sizes, test accuracy, and any categories that were left out.
6. Publish `model.bin` (version = current unix ms) through a per-process temp file → fsync → one rename → fsync(dir). Concurrent trainers and running apps only ever see a complete model.

## 7. Exit, signals, recovery

- **Esc / quit:** if the buffer is dirty, run one final save and wait for the reply. There is no timeout.
  - `Saved` → exit.
  - `Error` → stay open and show the error. Offer `e` (export the buffer to a recovery file) or `Q` (quit without saving, after a second confirmation).
  - `Conflict` → resolve it as in §5 first.
- **SIGTERM / SIGHUP** (for example, the terminal was closed): attempt the final save, then exit.
- **Panic:** restore the terminal, then exit. Edits since the last autosave are lost. This is the accepted loss window from §1.
- **Recovery files** are written only on the explicit export path. They go to `$XDG_DATA_HOME/quicknote/recovery/`, falling back to `~/.local/share/quicknote/recovery/`, with directory mode 0700 and file mode 0600. Write order:
  1. Write the whole buffer to a temp file in the same directory.
  2. `fsync(file)`.
  3. Rename it to a unique final name.
  4. `fsync(dir)`.

  Any failure is reported, and the buffer is kept.
- **Startup:** if recovery files exist, list them as `Recovered locally`. The user can import one as a new note. A recovery file is never merged into an existing note automatically.

## 8. Session, search, list

- `kv.active_note_id` and `notes.cursor` are saved with each save. On load, the cursor is clamped to the length of the text and moved back to a character boundary if needed.
- Empty notes (`body = ''`, not dirty) are deleted on normal exit and at startup.
- **Ctrl+F** opens a search overlay. It uses FTS5 ranked with `bm25(notes_fts)` and keeps the editor state. Search covers saved content only. Unsaved edits in the open note are not searchable until autosave runs, at most 5 s later.
- The note list is grouped by display category (§3). `Uncategorized` comes last.

**Status indicator:** `Unsaved` · `Saving` · `Saved` · `Conflict` · `Error: <reason>`. Any new edit clears `Saved` immediately.

## 9. Verification

Each item is one scripted or manual check:

1. **Process killed while typing** (`kill -9`): on restart, the note matches the last committed save and `PRAGMA integrity_check` returns `ok`.
2. **Two windows, same note:** both edit, the second save gets `Conflict`, and all three resolution options work without losing text.
3. **Database not writable** (read-only file, or disk full via a small tmpfs): `Error` is shown, the buffer is kept, Esc does not exit, and export writes a recovery file.
4. **600 KB paste:** rejected with a message, and the buffer is unchanged.
5. **Edit during inference:** the result for the old rev updates 0 rows, and the note is processed again at its new rev.
6. **No model yet:** the app works, notes are `Uncategorized`, and the status shows `auto-categorize: paused (no model yet, run quicknote train)`. Label at least 5 notes in each of 2 categories and run `quicknote train` while the app is open. The backlog drains within about 60 s.
9. **Too few labels:** `quicknote train` with fewer than 2 categories of 5 or more notes prints what's missing, exits with status 1, and writes no files.
7. **Change `model_version`:** every note without a manual category is processed again. Manual categories stay unchanged.
8. **Latency:** p95 keystroke-to-render stays under 16 ms while inference runs and while a save is fsyncing. Report the terminal, machine and note size used.

A process kill tests crash recovery. It does not prove the data survives a power loss, so durability claims stay limited to what §1 says.

## 10. Removed from v1.4

| v1.4 feature | Status in v2.0 | Add back when |
|---|---|---|
| Daemon, actor, systemd socket activation | Removed. One process per window | A feature truly needs shared live state across windows |
| Ownership tokens, Freeze/FreezeAck, Hyprland focus | Replaced by the `rev` check on save (§5) | — |
| stream_id/seq, dedup, DurableAck, retry buffer | Replaced by the SQLite commit result | — |
| Live keystroke mirroring, Lagged resync | Replaced by `data_version` polling and reload when clean | Live mirroring of the same note is used daily |
| IPC framing, 1 MiB frames, chunking | Removed (no IPC) | A daemon returns |
| Recovery file on every Esc timeout | Written only on explicit export after a failed save | — |
| `ai_events` table, intervention statuses, JSONL labeled export | Removed. `category_manual` vs `category_ai` already gives labeled pairs | Training or evaluating a model on user data is actually scheduled |
| "≥40 changed characters" inference trigger | Replaced by per-save `rev` plus the 4 s idle filter | — |
| ONNX runtime and tokenizer | Replaced by `rust_ann` `TextMlp` and hashed n-grams, trained by `quicknote train` | A bigger model clearly beats `TextMlp` on held-out notes |

## 11. Build order

1. Schema and migrations, the save thread, and the editor with autosave and status indicator. Checks 1, 3 and 4.
2. Note list, session restore, and FTS search.
3. Conflict handling and `data_version` reload. Check 2.
4. AI thread and backlog query. Checks 5, 6 and 7.
5. Model loading and `quicknote train`. Checks 6 and 9.
6. Latency measurement. Check 8.

## Decided defaults (adjust with real notes)

- The category set is the user's manual categories (§6).
- Confidence cutoff: `0.7`, set with `QUICKNOTE_MIN_CONFIDENCE`.
- Minimum to train: 5 notes per category, at least 2 categories.

## References

- SQLite WAL / synchronous: https://www.sqlite.org/pragma.html#pragma_synchronous
- SQLite data_version: https://www.sqlite.org/pragma.html#pragma_data_version
- SQLite FTS5 external content: https://www.sqlite.org/fts5.html#external_content_tables
- Linux fsync: https://man7.org/linux/man-pages/man2/fsync.2.html
- rust_ann (model library): `CNN-ANN_rust/README.md`, section "Text classification"
