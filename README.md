# QuickNote

**Version 1.0**

A terminal note app that sorts your notes into categories with a small neural network you train on your own notes. Notes live in SQLite and autosave as you type, and nothing leaves your machine.

## Install

```sh
cargo install --path .
quicknote
```

The model library [`rust_ann`](CNN-ANN_rust/) is a path dependency, so `CNN-ANN_rust/` must sit next to `Cargo.toml` when you build. `cargo install` puts the binary in `~/.cargo/bin`, so make sure that's on your `PATH`.

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

**Closing the terminal window** saves the current note before QuickNote exits.

## Categories and training

QuickNote 1.0 is tuned for four categories: `idea`, `kerja`, `meeting` and `research`. Use exactly these names with Ctrl+K so your labels join the training data.

```sh
quicknote train --extra data/train.tsv
```

Training shows a progress bar, then prints the accuracy and a confusion matrix: rows are the true category, columns are what the model predicted, and `recall` is the share of each category it got right. Within about a minute, the open app puts uncategorized notes into categories. A category you set by hand always wins.

- **`data/train.tsv`** is a balanced starter set with 200 examples per category, written in mixed Indonesian/English. It merges hand-written notes from two authors (`data/generated.tsv`, `data/codex_generated.tsv`) with research sections from a wiki. Rebuild it with `python3 data/build_train.py [wiki_dataset.zip]`. The format is one `category<TAB>text` line per example, so you can add your own.
- **Your own labels** are always added to the training data. Whenever the model gets a note wrong, fix it with Ctrl+K and train again.
- **Without the starter set,** `quicknote train` needs at least 5 labeled notes in each of at least 2 categories.
- **`--epochs N`** sets the training length (default 100). The loss settles within about 10 epochs, and 30 and 100 epochs scored the same (83.8%), so a shorter run like `--epochs 10` will likely do as well in a fraction of the time.

The model is a bag of character n-grams fed into a small MLP, which handles mixed Indonesian/English and typos. On the 160 held-out notes of the starter set it scores **83.8%**. The most common mistakes are between `kerja` and `meeting`.

Every training run is appended to `model/train.log` with its settings, the loss per epoch, the accuracy and the confusion matrix, so runs can be compared.

## Waybar

`contrib/waybar/quicknote-bar` prints the note count, with notes per category as the tooltip, as Waybar JSON. It reads the database read-only, so it's safe while QuickNote is open. It needs `sqlite3` and `jq`.

```sh
cp contrib/waybar/quicknote-bar ~/.local/bin/
```

Add this to your Waybar config and put `"custom/quicknote"` in a module list:

```jsonc
"custom/quicknote": {
  "exec": "~/.local/bin/quicknote-bar",
  "return-type": "json",
  "interval": 30,
  "signal": 9,
  "on-click": "kitty --class quicknote-panel ~/.cargo/bin/quicknote; pkill -RTMIN+9 waybar"
}
```

Clicking opens QuickNote, and quitting it refreshes the count. To make it a floating panel on Hyprland, add a window rule that floats the `quicknote-panel` class. 820×560 fits the 80-column layout. For an icon, put an `image#…` module in front of it, the same way Waybar's media modules pair an image with text.

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
- `model/model.bin`: the trained model, holding the weights, the category list and the version in one file.
- `model/train.log`: the history of training runs.
- `recovery/`: exported text from saves that failed.

## Version 1.0

- **Editor:** autosave, conflicts between windows resolved by revision, FTS5 search, recovery files, and a final save on Esc, SIGTERM or closing the window.
- **AI categorization:** `TextMlp` from `rust_ann` (pure Rust), a 4-category starter dataset, and `quicknote train` with `--extra`, `--epochs`, a progress bar, a confusion matrix and a training log.
- **Waybar module** (`contrib/waybar/`).
- **Tests:** 65, plus all 9 checks in the design spec (crash, disk full, conflicts, latency: p95 keystroke-to-render 0.23 ms).

## Development

```sh
cargo test
```

The design spec is `QuickNote_TDS_v2_0.md`, and the UI design is `docs/QuickNote_UI_UX.md`.
