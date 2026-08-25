use hane_document::{SourceOffset, SourceRange};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Selection {
    pub anchor: SourceOffset,
    pub active: SourceOffset,
}

impl Selection {
    pub fn caret(offset: SourceOffset) -> Self {
        Self {
            anchor: offset,
            active: offset,
        }
    }

    pub fn range(self) -> SourceRange {
        SourceRange {
            start: self.anchor.min(self.active),
            end: self.anchor.max(self.active),
        }
    }

    pub fn is_reversed(self) -> bool {
        self.active < self.anchor
    }
}
