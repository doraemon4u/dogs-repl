//! Minimal line editor for the bottom input bar.
//!
//! `reedline` is a whole line-oriented front end and does not compose well with
//! a full-screen `ratatui` loop, so the TUI uses this small editor instead. It
//! keeps the behaviour the design asks for: history, cursor movement for
//! editing, and UTF-8-safe insertion.

const HISTORY_CAPACITY: usize = 200;

#[derive(Debug, Default, Clone)]
pub struct InputState {
    chars: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    /// `Some(index)` while browsing history; the draft is saved on entry.
    history_index: Option<usize>,
    draft: String,
}

impl InputState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn buffer(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }

    /// Replaces the buffer and puts the cursor at the end (used by history).
    pub fn set_buffer(&mut self, text: impl Into<String>) {
        self.chars = text.into().chars().collect();
        self.cursor = self.chars.len();
    }

    pub fn insert(&mut self, ch: char) {
        self.chars.insert(self.cursor, ch);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, text: &str) {
        for ch in text.chars() {
            if ch == '\n' || ch == '\r' {
                continue;
            }
            self.insert(ch);
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        if self.cursor < self.chars.len() {
            self.cursor += 1;
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.chars.len();
    }

    /// Takes the submitted line and appends it to history when non-empty.
    pub fn submit(&mut self) -> String {
        let line = self.buffer();
        self.clear();
        self.history_index = None;
        self.draft.clear();
        if !line.trim().is_empty() && self.history.last().map(String::as_str) != Some(line.as_str())
        {
            self.history.push(line.clone());
            if self.history.len() > HISTORY_CAPACITY {
                self.history.remove(0);
            }
        }
        line
    }

    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_index {
            None => {
                self.draft = self.buffer();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(index) => index - 1,
        };
        self.history_index = Some(index);
        let entry = self.history[index].clone();
        self.set_buffer(entry);
    }

    pub fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 >= self.history.len() {
            self.history_index = None;
            let draft = self.draft.clone();
            self.set_buffer(draft);
        } else {
            self.history_index = Some(index + 1);
            let entry = self.history[index + 1].clone();
            self.set_buffer(entry);
        }
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_and_deletes_at_the_cursor() {
        let mut input = InputState::new();
        input.insert_str("abc");
        input.home();
        input.insert('X');
        assert_eq!(input.buffer(), "Xabc");
        input.right();
        input.backspace();
        assert_eq!(input.buffer(), "Xbc");
        input.delete();
        assert_eq!(input.buffer(), "Xc");
        input.end();
        input.insert('!');
        assert_eq!(input.buffer(), "Xc!");
    }

    #[test]
    fn submit_clears_and_records_history() {
        let mut input = InputState::new();
        input.insert_str("game 42");
        assert_eq!(input.submit(), "game 42");
        assert!(input.is_empty());
        assert_eq!(input.history(), ["game 42".to_string()]);
        // Duplicate consecutive entries are not stored twice.
        input.insert_str("game 42");
        input.submit();
        assert_eq!(input.history().len(), 1);
        // Empty lines are not stored.
        input.submit();
        assert_eq!(input.history().len(), 1);
    }

    #[test]
    fn history_browsing_restores_the_draft() {
        let mut input = InputState::new();
        for line in ["one", "two", "three"] {
            input.set_buffer(line);
            input.submit();
        }
        input.insert_str("draft");
        input.history_prev();
        assert_eq!(input.buffer(), "three");
        input.history_prev();
        assert_eq!(input.buffer(), "two");
        input.history_next();
        assert_eq!(input.buffer(), "three");
        input.history_next();
        assert_eq!(input.buffer(), "draft");
    }

    #[test]
    fn unicode_insertion_is_character_safe() {
        let mut input = InputState::new();
        input.insert_str("围棋");
        input.home();
        input.insert('!');
        assert_eq!(input.buffer(), "!围棋");
        input.backspace();
        assert_eq!(input.buffer(), "围棋");
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn paste_drops_newlines() {
        let mut input = InputState::new();
        input.insert_str("chat hello\nworld");
        assert_eq!(input.buffer(), "chat helloworld");
    }
}
