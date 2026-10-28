# QuickNote UI / UX

Based on [TDS v2.0](../QuickNote_TDS_v2_0.md) and [implementation plan, Tasks 5–8](superpowers/plans/2026-10-27-quicknote-v2.md). Existing copy and bindings are preserved. Additions and exceptions are listed under **Proposed changes**; mockups include those proposals.

## Screen layout

Coordinates use terminal cells. Frames are ASCII; `·`, `—` and `…` remain where the plan specifies them. `|` inside editor content illustrates the cursor, not stored text. Mockup note text, categories, filenames and reasons are examples.

### Editor: 100 × 30

Rows 1–29: borderless editor, one-cell horizontal padding, viewport follows cursor. Row 30: persistent status bar. No permanent sidebar or toolbar.

```text
 Project launch notes

 Bring the release checklist to Thursday's meeting.

 Follow up
 - Confirm the support rota
 - Write the announcement|






















Unsaved
```

One-line footer slots, in order: **status · app.message · AI status**. Omit empty slots and their separators. Idle/working AI copy is unspecified; leave that slot empty unless paused. Examples (each is an alternative footer):

```text
Saving
Saved
Unsaved · Note is limited to 512,000 bytes; edit rejected
Error: disk full · auto-categorize: paused (no model.onnx)
Error: disk full · Exported to /home/me/.local/share/quicknote/recovery/note-1.txt
```

The status bar stays visible beneath every overlay. Overlay interiors clear the underlying text; editor content outside the overlay remains in the terminal's default colors. Only the focused input shows a cursor.

### Ctrl+L: note list

Centered 76 × 18 panel at 100 × 30; excerpt below omits unused interior rows. `>` plus reverse video identifies the selected item. Headers are not selectable. Scroll items to keep selection visible.

```text
+-- Notes -----------------------------------------------------------------+
| Recovered locally                                                        |
| > note-2026-10-27-0900.txt                                               |
|                                                                          |
| Personal                                                                 |
|   Shopping list                                                          |
|                                                                          |
| Work                                                                     |
|   Project launch notes                                     Work (stale)  |
|   Release checklist                                                      |
|                                                                          |
| Uncategorized                                                            |
|   (empty)                                                                |
|                                                                          |
| Up/Down select · Enter open/import · Esc close                           |
+--------------------------------------------------------------------------+
```

Recovery files come first, only when present. Enter imports a recovery file as a **new** note; remove the file only after insertion succeeds. Then sort category groups by name, with `Uncategorized` last; newest updated note first within each group.

Use the first non-empty line as title, capped at 60 characters; fallback is `(empty)`. Manual category wins, then AI category, then `Uncategorized`. Show ` (stale)` on the affected note's category label, not on the entire group: only AI labels with `ai_rev < rev` qualify. Reserve space for that suffix before shortening titles or category names to fit.

### Ctrl+F: search

Centered 76 × 14 panel. Query input owns the cursor; Up/Down moves result selection without moving input focus. Results are snippets, ranked by relevance, at most 50; wrap each snippet to two lines before clipping. Keep literal `[` / `]` around matches and the planned `…` omission marker.

```text
+-- Search ----------------------------------------------------------------+
| > release|                                                               |
| Searches saved content only                                              |
|                                                                          |
| > Bring the [release] checklist to Thursday's meeting.                   |
|   …draft the [release] announcement before lunch…                        |
|   …review the [release] notes with support…                              |
|                                                                          |
| Up/Down select · Enter open · Esc close                                  |
+--------------------------------------------------------------------------+
```

Treat query input as plain text, including FTS punctuation. Opening the overlay preserves editor text and cursor. Unsaved edits become searchable only after saving; recovery files are not search results.

### Ctrl+K: category prompt

Centered 64 × 7 panel; free-text category, not a picker limited to model labels.

```text
+-- Category --------------------------------------------------+
| Category: Work|                                              |
|                                                              |
| Enter applies · Esc cancels                                  |
| Empty input clears manual category                           |
|                                                              |
+--------------------------------------------------------------+
```

Enter sets the manual category; empty input clears that override, allowing the AI label or `Uncategorized` to display. Keep text entry usable while auto-categorization is paused. This mockup shows typed input; prompt prefill is not specified by the plan.

### Conflict, QuitError and QuitConfirm

The QuitError template is `Error: <reason> — e export · Q quit without saving`. Keep the local editor buffer visible. These bars own keyboard focus; their letters are actions only in the corresponding mode. At 100 columns, short reasons and paused AI fit on one footer line:

```text
Conflict: r reload · o overwrite · n save as new · auto-categorize: paused (no model.onnx)
Error: disk full — e export · Q quit without saving · auto-categorize: paused (no model.onnx)
```

`r` discards local changes and loads the database version. `o` retries against the current database revision; a new race can conflict again. `n` inserts the local text as a new note. No option is automatic. If quitting caused the conflict, resolve it before completing quit.

In QuitError, `e` exports without exiting. Success displays `Exported to <path>`; failure displays the I/O error and retains the buffer. First uppercase `Q` opens QuitConfirm; only a second uppercase `Q` exits without a database save. Proposed confirmation bar:

```text
Quit without saving? Q confirms · Esc cancels
```

### Minimum: 80 × 24

Normal editor: rows 1–23; status: row 24. Overlays shrink to 72 columns maximum, stay centered above the footer, and scroll their content. Category prompt remains 64 × 7. Example full-screen list (24 rows, 80 columns maximum):

```text
 Project launch notes

 Bring the release checklist to Thursday's meeting.

    +-- Notes -------------------------------------------------------------+
    | Recovered locally                                                    |
    | > note-2026-10-27-0900.txt                                           |
    |                                                                      |
    | Personal                                                             |
    |   Shopping list                                                      |
    | Work                                                                 |
    |   Project launch notes                           Work (stale)        |
    |   Release checklist                                                  |
    | Uncategorized                                                        |
    |   (empty)                                                            |
    |                                                                      |
    | Up/Down select · Enter open/import · Esc close                       |
    +----------------------------------------------------------------------+





Saved · auto-categorize: paused (no model.onnx)
```

At 80 columns, Conflict plus paused AI cannot fit together. Proposed overflow footer uses rows 23–24 (editor becomes rows 1–22):

```text
Conflict: r reload · o overwrite · n save as new
auto-categorize: paused (no model.onnx)
```

QuitError uses the same rule:

```text
Error: disk full — e export · Q quit without saving
auto-categorize: paused (no model.onnx)
```

Wrap longer reasons/messages onto additional footer rows without changing their wording. Keep action keys visible, then message, then AI status; shrink/recenter overlays accordingly. For exceptionally long diagnostics, clip the dynamic value with `…` and provide the proposed detail view below. Never truncate the state label, conflict actions, quit actions or `(stale)` suffix. Clip by display-cell width, not byte count.

## Keybindings

| Mode | Key | Action |
|---|---|---|
| Edit | Printable characters / paste | Insert text; reject the entire edit if over 512,000 UTF-8 bytes |
| Edit | Enter / Tab | Insert newline / literal tab |
| Edit | Backspace / Delete | Delete before / at cursor |
| Edit | Arrows / Home / End | Move cursor / line start / line end |
| Edit | Ctrl+L | Open grouped note list |
| Edit | Ctrl+F | Open saved-content search |
| Edit | Ctrl+N | Create and open a new note |
| Edit | Ctrl+K | Open manual category prompt |
| Edit | Esc | Quit; if needed, save and wait for the current buffer generation |
| List / Search | Up / Down | Select previous / next item (proposed explicit binding) |
| List | Enter | Open note or import selected recovery file |
| Search | Text / Backspace / Delete / Left / Right / Home / End | Edit query (input editing details proposed) |
| Search | Enter | Open selected result |
| CategoryPrompt | Text / Backspace / Delete / Left / Right / Home / End | Edit category (input editing details proposed) |
| CategoryPrompt | Enter | Apply; empty input clears manual override |
| List / Search / CategoryPrompt | Esc | Close overlay, preserve editor state |
| Conflict | r / o / n | Reload / overwrite / save as new |
| QuitError | e | Export buffer; remain open |
| QuitError | Q | Enter QuitConfirm |
| QuitConfirm | Q | Confirm quit without saving |
| QuitConfirm | Esc | Return to QuitError (proposed) |
| Any mode | F1 | Proposed read-only help and full status details; Esc returns |

No manual-save shortcut is specified. Autosave runs after 1 second idle or 5 seconds dirty. New note, opening a result/note, and importing recovery wait for the current buffer to save; failure or conflict cancels the switch and retains the current note. Esc in an overlay never quits. Empty result lists have no Enter action.

## Visual states

Use terminal-default foreground/background for body text; avoid hardcoded black/white backgrounds and low-contrast dim text. Color is an optional accent on the status label only; every state retains its exact text. Use bold on labels and reverse video plus `>` on selection. No blinking, animation requirement, RGB, or color-only distinctions.

| Exact text | Meaning / transition | Optional ANSI 16-color accent |
|---|---|---|
| `Unsaved` | Local changes not yet committed; any new edit clears Saved immediately | Yellow on dark; default foreground on light |
| `Saving` | Save in flight; typing may continue in Edit | Cyan on dark; blue on light |
| `Saved` | Commit acknowledged for current generation only | Green on dark; default foreground on light |
| `Conflict` | Local buffer retained; explicit resolution required | Magenta |
| `Error: <reason>` | Failure retained until retry succeeds; never imply saved | Red |
| `auto-categorize: paused (<reason>)` | Independent AI state; editing, saving and manual categories work | Yellow on dark; default foreground on light |
| ` (stale)` | AI category describes an older saved revision | Default foreground; literal suffix |

Default to the terminal's foreground when its light/dark palette is unknown; enable accent colors only for a known readable palette. Monochrome uses the same text, bold and selection marker. Neither Saved nor stale needs an icon.

Status precedence follows Task 5: Conflict mode → save in flight → error → dirty → Saved. An acknowledgement for an older generation must not flash Saved. Paused AI never replaces the save status. A rejected edit leaves the buffer unchanged and displays `Note is limited to 512,000 bytes; edit rejected`.

## Empty states

New copy below is proposed. Hints are UI text, never part of the note body.

```text
Editor, first launch with no notes:
  No notes yet. Start typing.

List with no persisted notes or recovery entries:
  No notes yet. Esc returns to the editor.

Search, blank query:
  Type to search saved notes.

Search, nonblank query with no results:
  No matches in saved notes.

Category prompt when no categories exist:
  No categories yet. Type a category name.
```

Startup creates a blank editable note; if it appears in the list, show `(empty)` under `Uncategorized` instead of the no-notes list message. If only recovery files exist, show `Recovered locally` and its entries. Do not show empty category headers. “No categories yet” is informational: free-text assignment needs no existing labels. Do not claim no search hits while the query is blank.

## Proposed changes

No existing key is remapped and no specified copy string is replaced.

| Addition / exception | Reason |
|---|---|
| Add overlay titles `Notes`, `Search`, `Category`; field label `Category:` | The plan defines modes but leaves their visible names unspecified. |
| Add `Up/Down select · Enter open/import · Esc close` and `Up/Down select · Enter open · Esc close`; explicitly bind Up/Down | Make selection and recovery import discoverable; the plan does not define selection navigation. |
| Add `Searches saved content only` | Explain why very recent edits and recovery files are absent. |
| Add `Enter applies · Esc cancels` and `Empty input clears manual category`; define ordinary single-line input editing keys | Explain clearing an override before the user submits an empty value. |
| Add the five empty-state strings above | Give a next action where the plan supplies no copy. |
| Add `Quit without saving? Q confirms · Esc cancels`; Esc returns to QuitError | Make the required second confirmation visible and reversible. |
| Let the one-line footer expand on overflow, including at 80 columns | Preserve conflict/quit actions and paused AI together; they cannot always fit in one row. |
| Add F1 read-only `Help and status` overlay, closed with Esc; Up/Down scrolls | Expose existing shortcuts and full diagnostic reasons/export paths when they exceed available space. Opening/closing help preserves the underlying mode and buffer. |

F1 help should also state `r reload discards local changes`, `o overwrite replaces the database version`, and `n save as new keeps both versions`. These are proposed explanatory additions; the exact Conflict bar remains unchanged.

## Decisions (Claude, 2026-10-27)

These decisions are binding for implementation and override the proposals above where they conflict.

| Proposal | Decision |
|---|---|
| Overlay titles and the `Category:` label | **Accepted** |
| Up/Down selection plus the hint lines in the list and search overlays | **Accepted** |
| `Searches saved content only` | **Accepted** |
| Prompt hints `Enter applies · Esc cancels` / `Empty input clears manual category` | **Accepted** |
| Empty states | **Accepted**, except the category-prompt one (`No categories yet…`), which is **rejected**. The prompt is free text, so the line adds nothing and would need an extra query. |
| QuitConfirm bar `Quit without saving? Q confirms · Esc cancels`, with Esc returning to QuitError | **Accepted** |
| Multi-row overflow footer | **Rejected.** The footer stays one row. Slots keep their priority order (state/actions → message → AI status) and are clipped from the right by display width with `…`. State labels and action keys are never clipped; at 80 columns every action bar fits. The AI status shows again once the action bar clears. |
| F1 help overlay | **Rejected for v1.** It's a whole extra mode for discoverability. Instead, the empty-editor placeholder reads `No notes yet. Start typing. · Ctrl+L notes · Ctrl+F search · Ctrl+N new · Ctrl+K category · Esc quit`. Add F1 when a user asks where a shortcut lives. |
| Colors only "for a known readable palette" | **Changed.** Always use the named ANSI 16 colors from the Visual states table, because terminals theme those for light and dark backgrounds. Honor `NO_COLOR` (if the variable is set, use no colors; text, bold and `>` stay the same). |
