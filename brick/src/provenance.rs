use std::{fmt, sync::Arc};

#[derive(Clone, Hash, PartialEq, Eq)]
pub struct SourceRange {
    source_name: Arc<str>,
    source_text: Arc<str>,
    pub start_line: u32,
    pub start_offset: u32,
    pub end_line: u32,
    pub end_offset: u32,
}

impl SourceRange {
    pub fn new(start: SourceMarker, end: &SourceMarker) -> SourceRange {
        SourceRange::new_offset(start, end.line, end.offset)
    }

    pub fn new_offset(start: SourceMarker, end_line: u32, end_offset: u32) -> SourceRange {
        SourceRange {
            source_name: start.source_name,
            source_text: start.source_text,
            start_line: start.line,
            start_offset: start.offset,
            end_line,
            end_offset,
        }
    }

    pub fn start(&self) -> SourceMarker {
        SourceMarker {
            source_name: self.source_name.clone(),
            source_text: self.source_text.clone(),
            line: self.start_line,
            offset: self.start_offset,
        }
    }

    pub fn end(&self) -> SourceMarker {
        SourceMarker {
            source_name: self.source_name.clone(),
            source_text: self.source_text.clone(),
            line: self.end_line,
            offset: self.end_offset,
        }
    }

    pub fn set_end(&mut self, end: SourceMarker) {
        self.end_line = end.line;
        self.end_offset = end.offset;
    }

    fn is_one_char(&self) -> bool {
        self.start_line == self.end_line && self.start_offset == self.end_offset
    }

    pub fn text(&self) -> &str {
        let start = self.start().index();
        let end = self.end().index();
        &self.source_text[start..end]
    }

    pub fn contains(&self, line: u32, char: u32) -> bool {
        line >= self.start_line
            && line <= self.end_line
            && (line != self.start_line || char >= self.start_offset)
            && (line != self.end_line || char <= self.end_offset)
    }
}

impl fmt::Debug for SourceRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SourceRange {{ source_name: {}, start_line: {}, start_offset: {}, end_line: {}, end_offset: {} }}",
            self.source_name, self.start_line, self.start_offset, self.end_line, self.end_offset,
        )
    }
}

impl fmt::Display for SourceRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_one_char() {
            write!(
                f,
                "{}@{}:{}",
                self.source_name, self.start_line, self.start_offset
            )
        } else {
            write!(
                f,
                "{}@{}:{} - {}:{}",
                self.source_name,
                self.start_line,
                self.start_offset,
                self.end_line,
                self.end_offset
            )
        }
    }
}

#[derive(Clone, Hash, PartialEq, Eq)]
pub struct SourceMarker {
    source_name: Arc<str>,
    source_text: Arc<str>,
    line: u32,
    offset: u32,
}

impl SourceMarker {
    pub fn new(
        source_name: Arc<str>,
        source_text: Arc<str>,
        line: u32,
        offset: u32,
    ) -> SourceMarker {
        SourceMarker {
            source_name,
            source_text,
            line,
            offset,
        }
    }

    pub fn line(&self) -> u32 {
        self.line
    }

    pub fn offset(&self) -> u32 {
        self.offset
    }

    pub fn index(&self) -> usize {
        let mut index = 0;
        let mut current_line = 1;
        let mut chars = self.source_text.chars();
        while current_line < self.line {
            if chars.next().unwrap() == '\n' {
                current_line += 1;
            }
            index += 1;
        }

        index + (self.offset as usize)
    }

    pub fn to_range(&self) -> SourceRange {
        let line = self.line;
        let offset = self.offset;
        SourceRange::new_offset(self.clone(), line, offset)
    }
}

impl fmt::Debug for SourceMarker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SourceMarker {{ source_name: {}, line: {}, offset: {} }}",
            self.source_name, self.line, self.offset
        )
    }
}

impl fmt::Display for SourceMarker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}:{}", self.source_name, self.line, self.offset)
    }
}
