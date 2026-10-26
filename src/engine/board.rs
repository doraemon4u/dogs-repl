//! Go board data structure with coordinate system and basic operations.
//!
//! Supports 9x9, 13x13, and 19x19 boards. Columns skip letter 'I'.

/// Stone color on the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    Black,
    White,
}

impl Color {
    /// Returns the opponent's color.
    pub const fn opponent(self) -> Self {
        match self {
            Self::Black => Self::White,
            Self::White => Self::Black,
        }
    }
}

/// A Go board with a fixed size.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Board {
    size: u8,
    grid: Vec<Option<Color>>,
}

impl Board {
    /// Creates a new empty board of the given size (9, 13, or 19).
    pub fn new(size: u8) -> Self {
        let len = (size as usize) * (size as usize);
        Self {
            size,
            grid: vec![None; len],
        }
    }

    /// Returns the board size.
    pub const fn size(&self) -> u8 {
        self.size
    }

    /// Converts a column letter to a 0-based column index.
    /// Returns `None` for 'I' (skipped) or non-alphabetic characters.
    pub const fn col_index(letter: char) -> Option<u8> {
        let letter = letter.to_ascii_uppercase();
        if !letter.is_ascii_alphabetic() || letter == 'I' {
            return None;
        }
        Some(if letter > 'I' {
            letter as u8 - b'A' - 1
        } else {
            letter as u8 - b'A'
        })
    }

    /// Converts a 0-based column index back to a letter.
    pub const fn letter_for_col(col: u8) -> Option<char> {
        if col >= 19 {
            return None;
        }
        Some(if col < 8 {
            (b'A' + col) as char
        } else {
            (b'A' + col + 1) as char
        })
    }

    fn index(&self, col: u8, row: u16) -> Option<usize> {
        if col >= self.size || row == 0 || row > self.size as u16 {
            return None;
        }
        Some((row as usize - 1) * self.size as usize + col as usize)
    }

    /// Returns the stone at the given position, or `None` if empty or out of bounds.
    pub fn get(&self, col: u8, row: u16) -> Option<Color> {
        self.index(col, row).map(|idx| self.grid[idx]).flatten()
    }

    /// Places a stone at the given position.
    pub fn place(&mut self, col: u8, row: u16, color: Color) {
        if let Some(idx) = self.index(col, row) {
            self.grid[idx] = Some(color);
        }
    }

    /// Removes the stone at the given position.
    pub fn remove(&mut self, col: u8, row: u16) {
        if let Some(idx) = self.index(col, row) {
            self.grid[idx] = None;
        }
    }

    /// Returns `true` if the position is empty or out of bounds.
    pub fn is_empty(&self, col: u8, row: u16) -> bool {
        self.get(col, row).is_none()
    }

    /// Returns valid neighbor positions (up/down/left/right, within bounds).
    pub fn neighbors(&self, col: u8, row: u16) -> Vec<(u8, u16)> {
        let mut result = Vec::with_capacity(4);
        let deltas: [(i16, i16); 4] = [(0, -1), (0, 1), (-1, 0), (1, 0)];
        for (dc, dr) in deltas {
            let nc = col as i16 + dc;
            let nr = row as i16 + dr;
            if nc >= 0 && nc < self.size as i16 && nr >= 1 && nr <= self.size as i16 {
                result.push((nc as u8, nr as u16));
            }
        }
        result
    }

    /// Returns all positions of the given color on the board.
    pub fn stones_of(&self, color: Color) -> Vec<(u8, u16)> {
        let mut result = Vec::new();
        for row in 1..=self.size as u16 {
            for col in 0..self.size {
                if self.get(col, row) == Some(color) {
                    result.push((col, row));
                }
            }
        }
        result
    }

    /// Returns the total number of stones on the board.
    pub fn stone_count(&self) -> usize {
        self.grid.iter().filter(|s| s.is_some()).count()
    }

    /// Returns the canonical flat grid slice for positional superko comparison.
    pub fn grid_signature(&self) -> &[Option<Color>] {
        &self.grid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn col_index_basic() {
        assert_eq!(Board::col_index('A'), Some(0));
        assert_eq!(Board::col_index('H'), Some(7));
        assert_eq!(Board::col_index('J'), Some(8));
        assert_eq!(Board::col_index('T'), Some(18));
        assert_eq!(Board::col_index('a'), Some(0));
        assert_eq!(Board::col_index('I'), None);
        assert_eq!(Board::col_index('1'), None);
    }

    #[test]
    fn letter_for_col_basic() {
        assert_eq!(Board::letter_for_col(0), Some('A'));
        assert_eq!(Board::letter_for_col(7), Some('H'));
        assert_eq!(Board::letter_for_col(8), Some('J'));
        assert_eq!(Board::letter_for_col(18), Some('T'));
        assert_eq!(Board::letter_for_col(19), None);
    }

    #[test]
    fn board_new_empty() {
        let b9 = Board::new(9);
        assert_eq!(b9.size(), 9);
        assert_eq!(b9.stone_count(), 0);

        let b19 = Board::new(19);
        assert_eq!(b19.size(), 19);
        assert_eq!(b19.stone_count(), 0);
    }

    #[test]
    fn place_get_remove() {
        let mut b = Board::new(19);
        assert!(b.is_empty(0, 1));
        assert_eq!(b.get(0, 1), None);

        b.place(0, 1, Color::Black);
        assert!(!b.is_empty(0, 1));
        assert_eq!(b.get(0, 1), Some(Color::Black));

        b.remove(0, 1);
        assert!(b.is_empty(0, 1));
        assert_eq!(b.get(0, 1), None);
    }

    #[test]
    fn out_of_bounds() {
        let b = Board::new(9);
        assert_eq!(b.get(9, 1), None);
        assert_eq!(b.get(0, 10), None);
        assert_eq!(b.get(0, 0), None);
        assert!(b.is_empty(9, 1));
    }

    #[test]
    fn neighbors_center() {
        let b = Board::new(19);
        let n = b.neighbors(9, 10);
        assert_eq!(n.len(), 4);
        assert!(n.contains(&(9, 9)));
        assert!(n.contains(&(9, 11)));
        assert!(n.contains(&(8, 10)));
        assert!(n.contains(&(10, 10)));
    }

    #[test]
    fn neighbors_corner() {
        let b = Board::new(19);
        let n = b.neighbors(0, 1);
        assert_eq!(n.len(), 2);
        assert!(n.contains(&(0, 2)));
        assert!(n.contains(&(1, 1)));
    }

    #[test]
    fn stones_of() {
        let mut b = Board::new(9);
        b.place(0, 1, Color::Black);
        b.place(3, 4, Color::Black);
        b.place(5, 5, Color::White);

        let black = b.stones_of(Color::Black);
        assert_eq!(black.len(), 2);
        assert!(black.contains(&(0, 1)));
        assert!(black.contains(&(3, 4)));

        let white = b.stones_of(Color::White);
        assert_eq!(white.len(), 1);
        assert!(white.contains(&(5, 5)));
    }
}
