//! A terminal screen as far as asciier and its predecessor write one: cursor moves, clears, SGR attributes and colours, text, the alternate screen, DECSC/DECRC; APC (kitty graphics) and OSC strings are passed over, the title is kept.

/// One cell: its character and the pen it was written with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    pub pen: Pen,
}

/// SGR state: the foreground (`None` the default, 0-15 the theme's slots, `0x1_000_000 | rgb` truecolour), bold, dim, reverse.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Pen {
    pub fg: Option<u32>,
    pub bold: bool,
    pub dim: bool,
    pub reverse: bool,
}

const BLANK: Cell = Cell {
    ch: ' ',
    pen: Pen {
        fg: None,
        bold: false,
        dim: false,
        reverse: false,
    },
};

pub struct Screen {
    pub rows: usize,
    pub cols: usize,
    main: Vec<Vec<Cell>>,
    alt: Vec<Vec<Cell>>,
    in_alt: bool,
    row: usize,
    col: usize,
    /// The last column was written: the next character goes to the next row.
    wrap: bool,
    pen: Pen,
    saved: (usize, usize, Pen),
    saved_main: (usize, usize),
    pub title: String,
    /// The bytes of a sequence or character cut by the end of the last feed.
    pending: Vec<u8>,
}

impl Screen {
    pub fn new(rows: usize, cols: usize) -> Screen {
        Screen {
            rows,
            cols,
            main: vec![vec![BLANK; cols]; rows],
            alt: vec![vec![BLANK; cols]; rows],
            in_alt: false,
            row: 0,
            col: 0,
            wrap: false,
            pen: Pen::default(),
            saved: (0, 0, Pen::default()),
            saved_main: (0, 0),
            title: String::new(),
            pending: Vec::new(),
        }
    }

    /// A new window size: what is on screen stays where it fits.
    pub fn resize(&mut self, rows: usize, cols: usize) {
        for grid in [&mut self.main, &mut self.alt] {
            grid.resize(rows, vec![BLANK; cols]);
            for line in grid.iter_mut() {
                line.resize(cols, BLANK);
            }
        }
        (self.rows, self.cols) = (rows, cols);
        self.row = self.row.min(rows - 1);
        self.col = self.col.min(cols - 1);
    }

    fn grid(&mut self) -> &mut Vec<Vec<Cell>> {
        if self.in_alt {
            &mut self.alt
        } else {
            &mut self.main
        }
    }

    fn shown(&self) -> &Vec<Vec<Cell>> {
        if self.in_alt { &self.alt } else { &self.main }
    }

    /// Row `row` (1-based) as text, trailing blanks dropped.
    pub fn row(&self, row: usize) -> String {
        let text: String = self.shown()[row - 1].iter().map(|cell| cell.ch).collect();
        text.trim_end().to_string()
    }

    /// The cell at row `row`, column `col` (1-based).
    pub fn cell(&self, row: usize, col: usize) -> Cell {
        self.shown()[row - 1][col - 1]
    }

    /// Every row as text, one a line.
    pub fn text(&self) -> String {
        (1..=self.rows)
            .map(|row| self.row(row))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The rows (1-based) above the bottom row that hold anything but blanks.
    pub fn picture_rows(&self) -> Vec<usize> {
        (1..self.rows)
            .filter(|row| self.shown()[row - 1].iter().any(|cell| cell.ch != ' '))
            .collect()
    }

    /// The first and last column (1-based) holding anything but blanks above the bottom row.
    pub fn picture_cols(&self) -> Option<(usize, usize)> {
        let cols: Vec<usize> = (1..self.rows)
            .flat_map(|row| {
                let line = &self.shown()[row - 1];
                (1..=self.cols).filter(move |col| line[col - 1].ch != ' ')
            })
            .collect();
        Some((*cols.iter().min()?, *cols.iter().max()?))
    }

    /// Cells above the bottom row that hold anything but blanks.
    pub fn inked(&self) -> usize {
        self.shown()[..self.rows - 1]
            .iter()
            .flatten()
            .filter(|cell| cell.ch != ' ')
            .count()
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        let mut data = std::mem::take(&mut self.pending);
        data.extend_from_slice(bytes);
        let mut at = 0;
        while at < data.len() {
            match self.step(&data[at..]) {
                Some(used) => at += used,
                None => {
                    self.pending = data[at..].to_vec();
                    return;
                }
            }
        }
    }

    /// Act on the token `data` starts with; its length, `None` while it is cut short.
    fn step(&mut self, data: &[u8]) -> Option<usize> {
        let byte = data[0];
        match byte {
            0x1b => self.escape(data),
            b'\r' => {
                (self.col, self.wrap) = (0, false);
                Some(1)
            }
            b'\n' => {
                self.down();
                Some(1)
            }
            0x08 => {
                (self.col, self.wrap) = (self.col.saturating_sub(1), false);
                Some(1)
            }
            0..0x20 | 0x7f => Some(1),
            _ => {
                let len = match byte {
                    0xc0..0xe0 => 2,
                    0xe0..0xf0 => 3,
                    0xf0.. => 4,
                    _ => 1,
                };
                if data.len() < len {
                    return None;
                }
                let ch = std::str::from_utf8(&data[..len])
                    .ok()
                    .and_then(|text| text.chars().next())
                    .unwrap_or('\u{fffd}');
                self.put(ch);
                Some(len)
            }
        }
    }

    fn down(&mut self) {
        self.wrap = false;
        if self.row + 1 < self.rows {
            self.row += 1;
            return;
        }
        let cols = self.cols;
        let grid = self.grid();
        grid.remove(0);
        grid.push(vec![BLANK; cols]);
    }

    fn put(&mut self, ch: char) {
        if self.wrap {
            self.col = 0;
            self.down();
        }
        let (row, col, pen) = (self.row, self.col, self.pen);
        self.grid()[row][col] = Cell { ch, pen };
        if col + 1 < self.cols {
            self.col += 1;
        } else {
            self.wrap = true;
        }
    }

    fn escape(&mut self, data: &[u8]) -> Option<usize> {
        let kind = *data.get(1)?;
        match kind {
            b'[' => self.csi(data),
            // OSC: to BEL or ST
            b']' => {
                let body = &data[2..];
                let (end, len) = body.iter().enumerate().find_map(|(at, &b)| match b {
                    0x07 => Some((at, 1)),
                    0x1b if body.get(at + 1) == Some(&b'\\') => Some((at, 2)),
                    _ => None,
                })?;
                let text = String::from_utf8_lossy(&body[..end]);
                if let Some(title) = text.strip_prefix("2;").or(text.strip_prefix("0;")) {
                    self.title = title.to_string();
                }
                Some(2 + end + len)
            }
            // APC, DCS, PM: to ST
            b'_' | b'P' | b'^' => {
                let end = data[2..].windows(2).position(|pair| pair == b"\x1b\\")?;
                Some(2 + end + 2)
            }
            b'7' => {
                self.saved = (self.row, self.col, self.pen);
                Some(2)
            }
            b'8' => {
                (self.row, self.col, self.pen) = self.saved;
                self.wrap = false;
                Some(2)
            }
            b'(' | b')' => data.get(2).map(|_| 3),
            _ => Some(2),
        }
    }

    fn csi(&mut self, data: &[u8]) -> Option<usize> {
        let body = &data[2..];
        let end = body.iter().position(|b| (0x40..=0x7e).contains(b))?;
        let (params, last) = (&body[..end], body[end]);
        let private = params.first() == Some(&b'?');
        let text = String::from_utf8_lossy(if private { &params[1..] } else { params });
        let numbers: Vec<usize> = text.split(';').map(|n| n.parse().unwrap_or(0)).collect();
        let n = |at: usize, default: usize| {
            numbers
                .get(at)
                .copied()
                .filter(|&n| n != 0)
                .unwrap_or(default)
        };
        let (rows, cols) = (self.rows, self.cols);
        match last {
            b'H' | b'f' => {
                self.row = (n(0, 1) - 1).min(rows - 1);
                self.col = (n(1, 1) - 1).min(cols - 1);
                self.wrap = false;
            }
            b'A' => self.row = self.row.saturating_sub(n(0, 1)),
            b'B' => self.row = (self.row + n(0, 1)).min(rows - 1),
            b'C' => self.col = (self.col + n(0, 1)).min(cols - 1),
            b'D' => self.col = self.col.saturating_sub(n(0, 1)),
            b'G' => self.col = (n(0, 1) - 1).min(cols - 1),
            b'd' => self.row = (n(0, 1) - 1).min(rows - 1),
            b'J' => {
                let (row, col) = (self.row, self.col);
                let grid = self.grid();
                match numbers.first().copied().unwrap_or(0) {
                    0 => {
                        grid[row][col..].fill(BLANK);
                        grid[row + 1..].iter_mut().for_each(|line| line.fill(BLANK));
                    }
                    1 => {
                        grid[row][..=col].fill(BLANK);
                        grid[..row].iter_mut().for_each(|line| line.fill(BLANK));
                    }
                    _ => grid.iter_mut().for_each(|line| line.fill(BLANK)),
                }
            }
            b'K' => {
                let (row, col) = (self.row, self.col);
                let line = &mut self.grid()[row];
                match numbers.first().copied().unwrap_or(0) {
                    0 => line[col..].fill(BLANK),
                    1 => line[..=col].fill(BLANK),
                    _ => line.fill(BLANK),
                }
            }
            b'm' if !private => self.sgr(&numbers),
            b'h' | b'l' if private && numbers.contains(&1049) => {
                let enter = last == b'h';
                if enter && !self.in_alt {
                    self.saved_main = (self.row, self.col);
                    self.in_alt = true;
                    self.alt.iter_mut().for_each(|line| line.fill(BLANK));
                } else if !enter && self.in_alt {
                    self.in_alt = false;
                    (self.row, self.col) = self.saved_main;
                }
            }
            _ => {}
        }
        Some(2 + end + 1)
    }

    fn sgr(&mut self, numbers: &[usize]) {
        let mut at = 0;
        while at < numbers.len() {
            match numbers[at] {
                0 => self.pen = Pen::default(),
                1 => self.pen.bold = true,
                2 => self.pen.dim = true,
                22 => (self.pen.bold, self.pen.dim) = (false, false),
                7 => self.pen.reverse = true,
                27 => self.pen.reverse = false,
                code @ 30..=37 => self.pen.fg = Some(code as u32 - 30),
                code @ 90..=97 => self.pen.fg = Some(code as u32 - 90 + 8),
                39 => self.pen.fg = None,
                38 if numbers.get(at + 1) == Some(&2) => {
                    let part = |n: usize| numbers.get(at + n).copied().unwrap_or(0) as u32;
                    self.pen.fg = Some(0x100_0000 | part(2) << 16 | part(3) << 8 | part(4));
                    at += 4;
                }
                38 if numbers.get(at + 1) == Some(&5) => {
                    self.pen.fg = numbers.get(at + 2).map(|&n| n as u32);
                    at += 2;
                }
                _ => {}
            }
            at += 1;
        }
    }
}
