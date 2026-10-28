# QuickNote

A terminal note app that sorts your notes into categories with a small neural network you train on your own notes. Notes live in SQLite and autosave as you type, and nothing leaves your machine.

## Install

```sh
cargo install --path .
quicknote
```

The model library [`rust_ann`](CNN-ANN_rust/) is a path dependency, so `CNN-ANN_rust/` must sit next to `Cargo.toml` when you build.

## Keys

| Key | Action |
|---|---|
| Type, paste | Edit the note. It autosaves after 1 s of idle, or every 5 s while you keep typing |
| Ctrl+L | Note list, grouped by category (Up/Down, Enter opens) |
| Ctrl+F | Search saved notes (Up/Down, Enter opens) |
| Ctrl+N | New note |
| Ctrl+K | Set this note's category. Submitting it empty clears it |
| Esc | Close an overlay, or quit (waits for the final save) |

The bottom line shows the save state: `Unsaved` · `Saving` · `Saved` · `Conflict` · `Error: …`.

**Several windows** can be open at once. If two windows edit the same note, the later save shows `Conflict`: press `r` to reload theirs, `o` to overwrite with yours, or `n` to save yours as a new note. Nothing is discarded automatically.

**If saving fails** (disk full, for example), your text stays on screen and the app keeps retrying. If you quit then, press `e` to export the text to a recovery file, or `Q` twice to quit without saving. Recovery files appear at the top of the note list, and Enter imports one as a new note.

## Categories and training

QuickNote learns the categories you assign by hand:

1. Give at least **5 notes** in each of at least **2 categories** a category with Ctrl+K.
2. Run `quicknote train` (it's fine while the app is open). It prints the test accuracy.
3. Within about a minute, the open app puts uncategorized notes into categories. A category you set by hand always wins.

Run `quicknote train` again whenever you've labeled more notes. The model is a bag of character n-grams fed into a small MLP, which handles mixed Indonesian/English notes, and training takes seconds.

## Settings

| Variable | Default | Meaning |
|---|---|---|
| `QUICKNOTE_MIN_CONFIDENCE` | `0.7` | A prediction below this probability leaves the note `Uncategorized` |
| `XDG_DATA_HOME` | `~/.local/share` | Where the `quicknote/` data folder goes |
| `QUICKNOTE_LATENCY_LOG` | unset | Path for a per-keystroke render time log, in microseconds |
| `NO_COLOR` | unset | Disables colors |

## Files

All of it is under `$XDG_DATA_HOME/quicknote/`:

- `notes.db`: your notes (SQLite, WAL mode, `synchronous=FULL`).
- `model/model.bin`: the trained model.
- `recovery/`: exported text from saves that failed.

## Development

```sh
cargo test
```

The design spec is `QuickNote_TDS_v2_0.md`, and the UI design is `docs/QuickNote_UI_UX.md`.
