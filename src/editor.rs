use crate::db::MAX_NOTE_BYTES;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct TooLarge;

/// Editor text with a byte-offset cursor that always sits on a char boundary.
/// `gen` counts text changes; cursor moves don't touch it.
pub struct Buffer {
    text: String,
    cursor: usize,
    gen: u64,
}

impl Buffer {
    pub fn new(text: String, cursor: usize) -> Buffer {
        let mut cursor = cursor.min(text.len());
        while !text.is_char_boundary(cursor) {
            cursor -= 1;
        }
        Buffer { text, cursor, gen: 0 }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn gen(&self) -> u64 {
        self.gen
    }

    /// All-or-nothing: a paste that would exceed the note limit changes nothing.
    pub fn insert_str(&mut self, s: &str) -> Result<(), TooLarge> {
        if self.text.len() + s.len() > MAX_NOTE_BYTES {
            return Err(TooLarge);
        }
        if !s.is_empty() {
            self.text.insert_str(self.cursor, s);
            self.cursor += s.len();
            self.gen += 1;
        }
        Ok(())
    }

    pub fn backspace(&mut self) {
        if let Some(c) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= c.len_utf8();
            self.text.remove(self.cursor);
            self.gen += 1;
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
            self.gen += 1;
        }
    }

    pub fn left(&mut self) {
        if let Some(c) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= c.len_utf8();
        }
    }

    pub fn right(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start(self.cursor);
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end(self.cursor);
    }

    pub fn up(&mut self) {
        let start = self.line_start(self.cursor);
        if start > 0 {
            let col = self.column();
            self.cursor = self.at_column(self.line_start(start - 1), col);
        }
    }

    pub fn down(&mut self) {
        let end = self.line_end(self.cursor);
        if end < self.text.len() {
            let col = self.column();
            self.cursor = self.at_column(end + 1, col);
        }
    }

    fn line_start(&self, pos: usize) -> usize {
        self.text[..pos].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self, pos: usize) -> usize {
        self.text[pos..].find('\n').map_or(self.text.len(), |i| pos + i)
    }

    fn column(&self) -> usize {
        self.text[self.line_start(self.cursor)..self.cursor].chars().count()
    }

    /// Byte offset of char column `col` on the line starting at `start`, clamped to its end.
    fn at_column(&self, start: usize, col: usize) -> usize {
        let end = self.line_end(start);
        self.text[start..end]
            .char_indices()
            .nth(col)
            .map_or(end, |(i, _)| start + i)
    }
}

const IDLE: Duration = Duration::from_millis(1000);
const MAX_DIRTY: Duration = Duration::from_millis(5000);

/// When to autosave: after 1 s idle, or 5 s after the buffer first got dirty
/// (counted from the last successful save if typing never stopped).
#[derive(Default)]
pub struct Autosave {
    dirty_since: Option<Instant>,
    last_edit: Option<Instant>,
    in_flight: bool,
}

impl Autosave {
    pub fn on_edit(&mut self, now: Instant) {
        self.last_edit = Some(now);
        self.dirty_since.get_or_insert(now);
    }

    pub fn on_sent(&mut self) {
        self.in_flight = true;
    }

    pub fn on_saved(&mut self, now: Instant, still_dirty: bool) {
        self.in_flight = false;
        if still_dirty {
            self.dirty_since = Some(now);
        } else {
            self.dirty_since = None;
            self.last_edit = None;
        }
    }

    /// Retry 1 s later; the buffer stays dirty.
    pub fn on_failed(&mut self, now: Instant) {
        self.in_flight = false;
        self.last_edit = Some(now);
        self.dirty_since = Some(now);
    }

    pub fn due(&self, now: Instant) -> bool {
        match (self.in_flight, self.dirty_since, self.last_edit) {
            (false, Some(since), Some(edit)) => now - edit >= IDLE || now - since >= MAX_DIRTY,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::MAX_NOTE_BYTES;

    #[test]
    fn cursor_clamps_to_char_boundary() {
        assert_eq!(Buffer::new("héllo".into(), 2).cursor(), 1); // 2 is inside 'é'
        assert_eq!(Buffer::new("abc".into(), 99).cursor(), 3);
    }

    #[test]
    fn multibyte_insert_and_move() {
        let mut b = Buffer::new(String::new(), 0);
        b.insert_str("é").unwrap();
        assert_eq!((b.cursor(), b.gen()), (2, 1));
        b.left();
        assert_eq!((b.cursor(), b.gen()), (0, 1));
        b.delete();
        assert_eq!((b.text(), b.gen()), ("", 2));
    }

    #[test]
    fn insert_over_limit_rejected_whole() {
        let mut b = Buffer::new("a".repeat(MAX_NOTE_BYTES - 1), 0);
        assert!(b.insert_str("ab").is_err());
        assert_eq!((b.text().len(), b.gen()), (MAX_NOTE_BYTES - 1, 0));
        assert!(b.insert_str("a").is_ok());
    }

    #[test]
    fn up_down_keep_char_column() {
        let mut b = Buffer::new("abcd\nxy\nqrst".into(), 3);
        b.down();
        assert_eq!(b.cursor(), 7); // clamped to end of "xy"
        b.down();
        assert_eq!(b.cursor(), 10); // column 2 of "qrst"
    }

    #[test]
    fn autosave_idle_and_max_dirty() {
        let t = Instant::now();
        let ms = Duration::from_millis;
        let mut a = Autosave::default();
        assert!(!a.due(t));
        a.on_edit(t);
        assert!(!a.due(t + ms(999)));
        assert!(a.due(t + ms(1000)));
        let mut a = Autosave::default();
        for i in 0..10 {
            a.on_edit(t + ms(500 * i)); // continuous typing
        }
        assert!(!a.due(t + ms(4999)));
        assert!(a.due(t + ms(5000)));
        a.on_sent();
        assert!(!a.due(t + ms(9000))); // one save in flight at a time
        a.on_saved(t + ms(6000), false);
        assert!(!a.due(t + ms(60000)));
        a.on_edit(t + ms(60000));
        a.on_sent();
        a.on_failed(t + ms(60100));
        assert!(!a.due(t + ms(61099)));
        assert!(a.due(t + ms(61100)));
    }
}
