//! Tolerant Go coordinate parser and GTP helpers.
//!
//! Parses coordinates like `[a10]`, `[A10]`, `[a 10]`, `[q13]` into a [`Coord`]
//! struct. The parser is syntax-only — semantic validation (on-board check) is
//! the Board's responsibility — but it does reject the skipped column letter
//! `I`, because there is no unambiguous mapping for it.

/// A parsed Go coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Coord {
    /// Column index (0-based, skips 'I').
    pub col: u8,
    /// Row number (1-based, from bottom).
    pub row: u16,
}

impl Coord {
    pub const fn new(col: u8, row: u16) -> Self {
        Self { col, row }
    }

    /// Returns `true` when this coordinate is inside a board of `size`.
    pub const fn is_on_board(self, size: u8) -> bool {
        self.col < size && self.row >= 1 && self.row <= size as u16
    }

    /// Column letter for this coordinate (`A..H, J..`).
    pub const fn letter(self) -> char {
        let letter = if self.col < 8 {
            (b'A' + self.col) as char
        } else {
            (b'A' + self.col + 1) as char
        };
        letter
    }

    /// Formats the coordinate in GTP form (column letter + row from bottom),
    /// for example `D4`.
    pub fn to_gtp(self) -> String {
        format!("{}{}", self.letter(), self.row)
    }

    /// Parses a GTP coordinate such as `D4` or `q16` (case-insensitive).
    pub fn from_gtp(input: &str) -> Result<Self, ParseError> {
        parse_coord(&format!("[{}]", input.trim()))
    }
}

impl std::fmt::Display for Coord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}{}]", self.letter(), self.row)
    }
}

/// Errors that can occur during coordinate parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// Input does not start with `[` and end with `]`.
    MissingBrackets,
    /// Brackets are present but contain nothing.
    Empty,
    /// First character inside brackets is not an ASCII letter.
    InvalidLetter,
    /// The letter is `I`, which Go notation skips.
    SkippedLetter,
    /// Characters after the letter are not all digits.
    InvalidNumber,
    /// The numeric part does not fit in a `u16`.
    NumberOverflow,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingBrackets => write!(f, "missing brackets (expected [a10])"),
            Self::Empty => write!(f, "empty brackets"),
            Self::InvalidLetter => write!(f, "invalid column letter"),
            Self::SkippedLetter => write!(f, "column letter I is skipped in Go notation"),
            Self::InvalidNumber => write!(f, "invalid row number"),
            Self::NumberOverflow => write!(f, "row number is too large"),
        }
    }
}

impl std::error::Error for ParseError {}

/// The syntactic pieces of a coordinate, before any board-size validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawCoord {
    pub letter: char,
    pub number: u16,
}

/// Parse the raw letter/number pair, without mapping it to a column index.
pub fn parse_raw(input: &str) -> Result<RawCoord, ParseError> {
    let trimmed = input.trim();
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .ok_or(ParseError::MissingBrackets)?;

    let mut chars = inner.trim().chars();
    let letter = chars.next().ok_or(ParseError::Empty)?;
    if !letter.is_ascii_alphabetic() {
        return Err(ParseError::InvalidLetter);
    }

    let rest: String = chars.skip_while(|c| c.is_whitespace()).collect();
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return Err(ParseError::InvalidNumber);
    }

    let number = rest.parse::<u16>().map_err(|_| ParseError::NumberOverflow)?;
    Ok(RawCoord {
        letter: letter.to_ascii_uppercase(),
        number,
    })
}

/// Parse a coordinate string like `[a10]` or `[Q 13]`.
///
/// Accepts arbitrary whitespace (including none) between the letter and the
/// number, and is case-insensitive.
///
/// # Errors
/// Returns [`ParseError`] if the input is malformed or uses the skipped
/// column letter `I`.
pub fn parse_coord(input: &str) -> Result<Coord, ParseError> {
    let raw = parse_raw(input)?;
    if raw.letter == 'I' {
        return Err(ParseError::SkippedLetter);
    }
    let col = if raw.letter > 'I' {
        raw.letter as u8 - b'A' - 1
    } else {
        raw.letter as u8 - b'A'
    };
    Ok(Coord {
        col,
        row: raw.number,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple() {
        assert_eq!(parse_coord("[a10]").unwrap(), Coord { col: 0, row: 10 });
        assert_eq!(parse_coord("[A10]").unwrap(), Coord { col: 0, row: 10 });
        assert_eq!(parse_coord("[q13]").unwrap(), Coord { col: 15, row: 13 });
    }

    #[test]
    fn parse_with_whitespace() {
        assert_eq!(parse_coord("[a 10]").unwrap(), Coord { col: 0, row: 10 });
        assert_eq!(
            parse_coord("[a          10]").unwrap(),
            Coord { col: 0, row: 10 }
        );
        assert_eq!(parse_coord("[a10  ]").unwrap(), Coord { col: 0, row: 10 });
        assert_eq!(parse_coord("[  a 10  ]").unwrap(), Coord { col: 0, row: 10 });
    }

    #[test]
    fn parse_tolerates_a_hundred_spaces() {
        let input = format!("[a{}10]", " ".repeat(120));
        assert_eq!(parse_coord(&input).unwrap(), Coord { col: 0, row: 10 });
    }

    #[test]
    fn parse_skips_i() {
        assert_eq!(parse_coord("[J1]").unwrap(), Coord { col: 8, row: 1 });
        assert_eq!(parse_coord("[T19]").unwrap(), Coord { col: 18, row: 19 });
        assert_eq!(parse_coord("[I10]"), Err(ParseError::SkippedLetter));
        assert_eq!(parse_coord("[i10]"), Err(ParseError::SkippedLetter));
    }

    #[test]
    fn parse_errors() {
        assert_eq!(parse_coord("[ ]"), Err(ParseError::Empty));
        assert_eq!(parse_coord("[]"), Err(ParseError::Empty));
        assert_eq!(parse_coord("[ab]"), Err(ParseError::InvalidNumber));
        assert_eq!(parse_coord("[a10x]"), Err(ParseError::InvalidNumber));
        assert_eq!(parse_coord("a10"), Err(ParseError::MissingBrackets));
        assert_eq!(parse_coord("[1a]"), Err(ParseError::InvalidLetter));
        assert_eq!(parse_coord(""), Err(ParseError::MissingBrackets));
        assert_eq!(parse_coord("[a999999999]"), Err(ParseError::NumberOverflow));
    }

    #[test]
    fn raw_coord_keeps_the_letter() {
        assert_eq!(
            parse_raw("[i10]").unwrap(),
            RawCoord {
                letter: 'I',
                number: 10
            }
        );
    }

    #[test]
    fn gtp_roundtrip_skips_i() {
        assert_eq!(Coord::new(0, 10).to_gtp(), "A10");
        assert_eq!(Coord::new(8, 1).to_gtp(), "J1");
        assert_eq!(Coord::new(18, 19).to_gtp(), "T19");
        assert_eq!(
            Coord::from_gtp(&Coord::new(15, 4).to_gtp()).unwrap(),
            Coord::new(15, 4)
        );
        assert_eq!(Coord::from_gtp("d4").unwrap(), Coord::new(3, 4));
        assert_eq!(Coord::from_gtp("I4"), Err(ParseError::SkippedLetter));
    }

    #[test]
    fn on_board_check() {
        assert!(Coord::new(0, 1).is_on_board(9));
        assert!(Coord::new(8, 9).is_on_board(9));
        assert!(!Coord::new(9, 9).is_on_board(9));
        assert!(!Coord::new(0, 10).is_on_board(9));
        assert!(!Coord::new(0, 0).is_on_board(9));
    }
}
