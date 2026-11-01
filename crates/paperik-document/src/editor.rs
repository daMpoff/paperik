//! In-memory editing. The immutable source package is never rewritten here.
use crate::{Document, Font, Paragraph, Run};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
use unicode_segmentation::UnicodeSegmentation;
mod recovery;

const MAX_TEXT: usize = 16 * 1024 * 1024;
const MAX_PARAGRAPH: usize = 200_000;
const MAX_PARAGRAPHS: usize = 10_000;
const MAX_HISTORY: usize = 256;
const HISTORY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub paragraph: usize,
    /// UTF-16 code units, always at an extended grapheme boundary.
    pub offset: u32,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Position,
    pub focus: Position,
}
impl Selection {
    pub fn range(self) -> (Position, Position) {
        (self.anchor.min(self.focus), self.anchor.max(self.focus))
    }
    pub fn collapsed(self) -> bool {
        self.anchor == self.focus
    }
}
pub enum Edit {
    Insert(String),
    SoftBreak,
    Backspace { word: bool },
    Delete { word: bool },
    Undo,
    Redo,
}
#[derive(Clone)]
struct Snapshot {
    document: Arc<Document>,
    selection: Selection,
    revision: u64,
}
enum ChangeKind {
    New,
    Undo,
    Redo,
}
/// Build layout for this document first; only commit if layout succeeds.
pub struct PendingChange {
    pub document: Document,
    selection: Selection,
    generation: u64,
    revision: u64,
    kind: ChangeKind,
}
#[derive(Clone)]
pub struct Editor {
    document: Arc<Document>,
    selection: Selection,
    undo: VecDeque<Snapshot>,
    redo: VecDeque<Snapshot>,
    generation: u64,
    revision: u64,
    next_revision: u64,
    saved_revision: u64,
}

pub fn utf16_len(text: &str) -> u32 {
    text.encode_utf16().count() as u32
}
fn byte_at(text: &str, offset: u32) -> usize {
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        if units >= offset {
            return byte;
        }
        units += ch.len_utf16() as u32;
    }
    text.len()
}
fn boundaries(text: &str) -> Vec<u32> {
    let mut result = vec![0];
    let mut length = 0;
    for cluster in text.graphemes(true) {
        length += utf16_len(cluster);
        result.push(length);
    }
    result
}
pub fn snap(text: &str, offset: u32) -> u32 {
    boundaries(text)
        .into_iter()
        .min_by_key(|value| value.abs_diff(offset))
        .unwrap_or(0)
}
fn neighbour(text: &str, offset: u32, forward: bool, word: bool) -> u32 {
    if word {
        let words: Vec<(u32, u32)> = text
            .unicode_word_indices()
            .map(|(byte, value)| {
                let start = utf16_len(&text[..byte]);
                (start, start + utf16_len(value))
            })
            .collect();
        if forward {
            return words
                .iter()
                .find(|(start, _)| *start > offset)
                .map_or(utf16_len(text), |(start, _)| *start);
        }
        return words
            .iter()
            .rev()
            .find(|(start, _)| *start < offset)
            .map_or(0, |(start, _)| *start);
    }
    let all = boundaries(text);
    if forward {
        all.into_iter()
            .find(|value| *value > offset)
            .unwrap_or(offset)
    } else {
        all.into_iter()
            .rev()
            .find(|value| *value < offset)
            .unwrap_or(0)
    }
}
fn slice_runs(paragraph: &Paragraph, start: u32, end: u32) -> Vec<Run> {
    let mut offset = 0;
    let mut result = Vec::new();
    for run in &paragraph.runs {
        let length = utf16_len(&run.text);
        let a = start.saturating_sub(offset).min(length);
        let b = end.saturating_sub(offset).min(length);
        if a < b {
            result.push(Run {
                text: run.text[byte_at(&run.text, a)..byte_at(&run.text, b)].into(),
                font: run.font.clone(),
                source: run.source,
            });
        }
        offset += length;
    }
    result
}
fn insertion_font(paragraph: &Paragraph, offset: u32) -> (Font, Option<(usize, usize)>) {
    let mut end = 0;
    for run in &paragraph.runs {
        end += utf16_len(&run.text);
        if offset <= end && !run.text.is_empty() {
            return (run.font.clone(), run.source);
        }
    }
    (paragraph.mark_font.clone(), None)
}
fn compact(runs: Vec<Run>) -> Vec<Run> {
    let mut result: Vec<Run> = Vec::new();
    for run in runs.into_iter().filter(|r| !r.text.is_empty()) {
        if let Some(last) = result.last_mut()
            && last.font == run.font
            && last.source == run.source
        {
            last.text.push_str(&run.text);
        } else {
            result.push(run);
        }
    }
    result
}
fn renumber(document: &mut Document) {
    let mut counters = BTreeMap::new();
    for paragraph in &mut document.paragraphs {
        if let Some(list) = &paragraph.list
            && list.decimal
        {
            let counter = counters
                .entry((list.id.clone(), list.level))
                .or_insert(list.start);
            paragraph.list_label = Some(
                list.pattern
                    .replace(&format!("%{}", list.level + 1), &counter.to_string()),
            );
            *counter = counter.saturating_add(1);
        }
    }
}
fn validate(document: &Document) -> Result<(), String> {
    if document.paragraphs.len() > MAX_PARAGRAPHS
        || document
            .paragraphs
            .iter()
            .any(|p| p.runs.iter().map(|r| r.text.len()).sum::<usize>() > MAX_PARAGRAPH)
        || document
            .paragraphs
            .iter()
            .flat_map(|p| &p.runs)
            .map(|r| r.text.len())
            .sum::<usize>()
            > MAX_TEXT
    {
        return Err("Превышен предел редактирования: 16 МиБ текста, 10 000 абзацев или 200 000 байт в абзаце".into());
    }
    Ok(())
}
fn snapshot_bytes(snapshot: &Snapshot) -> usize {
    // Include owned formatting strings and vectors, not the shared original ZIP.
    snapshot
        .document
        .paragraphs
        .iter()
        .chain(&snapshot.document.footer)
        .map(|p| {
            std::mem::size_of::<Paragraph>()
                + p.style_id.len()
                + p.mark_font.family.len()
                + p.runs
                    .iter()
                    .map(|r| std::mem::size_of::<Run>() + r.text.len() + r.font.family.len())
                    .sum::<usize>()
                + p.style
                    .tabs
                    .iter()
                    .map(|(_, s)| 32 + s.len())
                    .sum::<usize>()
                + p.list
                    .as_ref()
                    .map_or(0, |l| 64 + l.id.len() + l.pattern.len())
                + p.list_label.as_ref().map_or(0, String::len)
        })
        .sum()
}
impl Editor {
    pub fn new(mut document: Document) -> Self {
        if document.paragraphs.is_empty() {
            document.paragraphs.push(Paragraph {
                runs: Vec::new(),
                style: Default::default(),
                mark_font: Font::default(),
                style_id: String::new(),
                list_label: None,
                list: None,
                page_field: false,
                source: None,
                edit_block: None,
            });
        }
        Self {
            document: Arc::new(document),
            selection: Selection::default(),
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            generation: 0,
            revision: 0,
            next_revision: 1,
            saved_revision: 0,
        }
    }
    pub fn document(&self) -> &Document {
        &self.document
    }
    pub fn selection(&self) -> Selection {
        self.selection
    }
    pub fn dirty(&self) -> bool {
        self.revision != self.saved_revision
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn checkpoint_stamp(&self) -> u64 {
        self.generation
    }
    pub fn mark_saved(&mut self) {
        self.saved_revision = self.revision;
    }
    pub fn end(&self) -> Position {
        let index = self.document.paragraphs.len().saturating_sub(1);
        Position {
            paragraph: index,
            offset: self
                .document
                .paragraphs
                .get(index)
                .map_or(0, |p| utf16_len(&p.text())),
        }
    }
    pub fn set_position(&mut self, mut position: Position, extend: bool) {
        position.paragraph = position
            .paragraph
            .min(self.document.paragraphs.len().saturating_sub(1));
        position.offset = self
            .document
            .paragraphs
            .get(position.paragraph)
            .map_or(0, |p| snap(&p.text(), position.offset));
        self.selection.focus = position;
        if !extend {
            self.selection.anchor = position;
        }
        self.generation += 1;
    }
    pub fn select_all(&mut self) {
        self.selection = Selection {
            anchor: Position::default(),
            focus: self.end(),
        };
        self.generation += 1;
    }
    pub fn select_word(&mut self, position: Position) {
        self.set_position(position, false);
        let position = self.selection.focus;
        let text = self.document.paragraphs[position.paragraph].text();
        let byte = byte_at(&text, position.offset);
        if let Some((start, word)) = text
            .split_word_bound_indices()
            .find(|(start, value)| *start <= byte && byte < *start + value.len())
        {
            self.selection.anchor.offset = utf16_len(&text[..start]);
            self.selection.focus.offset = self.selection.anchor.offset + utf16_len(word);
        }
        self.generation += 1;
    }
    pub fn selected_text(&self) -> String {
        let (start, end) = self.selection.range();
        if self.selection.collapsed() {
            return String::new();
        }
        (start.paragraph..=end.paragraph)
            .map(|index| {
                let text = self.document.paragraphs[index].text();
                let a = if index == start.paragraph {
                    byte_at(&text, start.offset)
                } else {
                    0
                };
                let b = if index == end.paragraph {
                    byte_at(&text, end.offset)
                } else {
                    text.len()
                };
                text[a..b].to_owned()
            })
            .collect::<Vec<_>>()
            .join("\r\n")
    }
    fn adjacent(&self, position: Position, forward: bool, word: bool) -> Position {
        let text = self.document.paragraphs[position.paragraph].text();
        if forward
            && position.offset == utf16_len(&text)
            && position.paragraph + 1 < self.document.paragraphs.len()
        {
            Position {
                paragraph: position.paragraph + 1,
                offset: 0,
            }
        } else if !forward && position.offset == 0 && position.paragraph > 0 {
            Position {
                paragraph: position.paragraph - 1,
                offset: utf16_len(&self.document.paragraphs[position.paragraph - 1].text()),
            }
        } else {
            Position {
                offset: neighbour(&text, position.offset, forward, word),
                ..position
            }
        }
    }
    pub fn move_horizontal(&mut self, forward: bool, extend: bool, word: bool) {
        let position = if !extend && !self.selection.collapsed() {
            let (a, b) = self.selection.range();
            if forward { b } else { a }
        } else {
            self.adjacent(self.selection.focus, forward, word)
        };
        self.set_position(position, extend);
    }
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            document: self.document.clone(),
            selection: self.selection,
            revision: self.revision,
        }
    }
    pub fn prepare(&self, edit: Edit) -> Result<Option<PendingChange>, String> {
        let (kind, history) = match edit {
            Edit::Undo => (ChangeKind::Undo, self.undo.back()),
            Edit::Redo => (ChangeKind::Redo, self.redo.back()),
            _ => (ChangeKind::New, None),
        };
        if !matches!(kind, ChangeKind::New) {
            return Ok(history.map(|snapshot| PendingChange {
                document: snapshot.document.as_ref().clone(),
                selection: snapshot.selection,
                generation: self.generation,
                revision: snapshot.revision,
                kind,
            }));
        }
        if self.document.paragraphs.is_empty() {
            return Err("Нет редактируемых абзацев".into());
        }
        let mut selection = self.selection;
        let mut soft = false;
        let input = match edit {
            Edit::Insert(value) => value.replace("\r\n", "\n").replace('\r', "\n"),
            Edit::SoftBreak => {
                soft = true;
                "\n".into()
            }
            Edit::Backspace { word } | Edit::Delete { word } => {
                if selection.collapsed() {
                    selection.anchor =
                        self.adjacent(selection.focus, matches!(edit, Edit::Delete { .. }), word);
                }
                String::new()
            }
            Edit::Undo | Edit::Redo => unreachable!(),
        };
        if input.len() > MAX_TEXT
            || input.chars().any(|c| {
                (c.is_control() && c != '\n' && c != '\t') || matches!(c, '\u{fffe}' | '\u{ffff}')
            })
        {
            return Err(
                "Вставка слишком велика или содержит неподдерживаемые управляющие символы".into(),
            );
        }
        let (start, end) = selection.range();
        if start == end && input.is_empty() {
            return Ok(None);
        }
        let breaks = if soft {
            0
        } else {
            input.bytes().filter(|b| *b == b'\n').count()
        };
        if self.document.paragraphs.len() - (end.paragraph - start.paragraph) + breaks
            > MAX_PARAGRAPHS
        {
            return Err("Вставка создаёт слишком много абзацев (предел — 10 000)".into());
        }
        if self.document.paragraphs[start.paragraph..=end.paragraph]
            .iter()
            .any(|p| p.page_field)
        {
            return Err("Редактирование полей документа пока не поддерживается".into());
        }
        if let Some(reason) = self.document.paragraphs[start.paragraph..=end.paragraph]
            .iter()
            .find_map(|p| p.edit_block)
        {
            return Err(format!("Этот абзац доступен только для чтения: {reason}"));
        }
        let mut document = self.document.as_ref().clone();
        let first = &self.document.paragraphs[start.paragraph];
        let last = &self.document.paragraphs[end.paragraph];
        let (font, source) = insertion_font(first, start.offset);
        let before = slice_runs(first, 0, start.offset);
        let after = slice_runs(last, end.offset, utf16_len(&last.text()));
        let chunks: Vec<&str> = if soft {
            vec![&input]
        } else {
            input.split('\n').collect()
        };
        let mut replacements = Vec::new();
        let mut focus = start;
        for (index, text) in chunks.iter().enumerate() {
            let mut paragraph = first.clone();
            paragraph.runs = if index == 0 {
                before.clone()
            } else {
                Vec::new()
            };
            if index > 0 {
                paragraph.style.page_break = false;
            }
            paragraph.runs.push(Run {
                text: (*text).into(),
                font: font.clone(),
                source,
            });
            if index + 1 == chunks.len() {
                focus = Position {
                    paragraph: start.paragraph + index,
                    offset: (if index == 0 { start.offset } else { 0 }) + utf16_len(text),
                };
                paragraph.runs.extend(after.clone());
            }
            paragraph.runs = compact(paragraph.runs);
            replacements.push(paragraph);
        }
        document
            .paragraphs
            .splice(start.paragraph..=end.paragraph, replacements);
        validate(&document)?;
        renumber(&mut document);
        if document.paragraphs == self.document.paragraphs {
            return Ok(None);
        }
        Ok(Some(PendingChange {
            document,
            selection: Selection {
                anchor: focus,
                focus,
            },
            generation: self.generation,
            revision: self.next_revision,
            kind,
        }))
    }
    pub fn commit(&mut self, change: PendingChange) -> Result<(), String> {
        if change.generation != self.generation {
            return Err("Команда относится к устаревшей версии документа".into());
        }
        let previous = self.snapshot();
        match change.kind {
            ChangeKind::New => {
                self.undo.push_back(previous);
                self.redo.clear();
                self.next_revision += 1;
            }
            ChangeKind::Undo => {
                self.undo.pop_back();
                self.redo.push_back(previous);
            }
            ChangeKind::Redo => {
                self.redo.pop_back();
                self.undo.push_back(previous);
            }
        }
        self.document = Arc::new(change.document);
        self.selection = change.selection;
        self.revision = change.revision;
        self.generation += 1;
        // A newly inserted combining mark can merge with the next grapheme.
        let text = self.document.paragraphs[self.selection.focus.paragraph].text();
        let collapsed = self.selection.collapsed();
        self.selection.focus.offset = snap(&text, self.selection.focus.offset);
        if collapsed {
            self.selection.anchor = self.selection.focus;
        }
        while self.undo.len() + self.redo.len() > MAX_HISTORY
            || self
                .undo
                .iter()
                .chain(&self.redo)
                .map(snapshot_bytes)
                .sum::<usize>()
                > HISTORY_BYTES
        {
            if self.undo.pop_front().is_none() {
                self.redo.pop_front();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn editor() -> Editor {
        let document = Document::from_bytes(
            include_bytes!("../../../tests/fixtures/formatting.docx").to_vec(),
        )
        .unwrap();
        Editor::new(document)
    }
    fn apply(editor: &mut Editor, edit: Edit) {
        if let Some(change) = editor.prepare(edit).unwrap() {
            editor.commit(change).unwrap();
        }
    }
    fn plain(text: &str) -> Editor {
        let mut e = editor();
        e.select_all();
        apply(&mut e, Edit::Insert(text.into()));
        e
    }
    #[test]
    fn unicode_deletion_treats_extended_graphemes_as_units() {
        for cluster in ["е\u{301}", "🇷🇺", "👩🏽‍💻", "𐐀"] {
            let mut e = plain(&format!("a{cluster}z"));
            e.set_position(
                Position {
                    paragraph: 0,
                    offset: 1 + utf16_len(cluster),
                },
                false,
            );
            apply(&mut e, Edit::Backspace { word: false });
            assert_eq!(e.document.paragraphs[0].text(), "az");
            apply(&mut e, Edit::Undo);
            e.set_position(
                Position {
                    paragraph: 0,
                    offset: 1,
                },
                false,
            );
            apply(&mut e, Edit::Delete { word: false });
            assert_eq!(e.document.paragraphs[0].text(), "az");
        }
    }
    #[test]
    fn split_join_and_paste_keep_styles_and_source() {
        let mut e = editor();
        let original = e.document.package.original.clone();
        let style = e.document.paragraphs[0].style.clone();
        e.set_position(
            Position {
                paragraph: 0,
                offset: 0,
            },
            false,
        );
        apply(&mut e, Edit::Insert("alpha\r\nbeta\rgamma".into()));
        assert_eq!(e.document.paragraphs[0].text(), "alpha");
        assert!(e.document.paragraphs[2].text().starts_with("gamma"));
        assert_eq!(e.document.paragraphs[1].style, style);
        e.set_position(
            Position {
                paragraph: 1,
                offset: 0,
            },
            false,
        );
        apply(&mut e, Edit::Backspace { word: false });
        assert_eq!(e.document.paragraphs[0].text(), "alphabeta");
        assert!(std::sync::Arc::ptr_eq(
            &original,
            &e.document.package.original
        ));
    }
    #[test]
    fn replacement_preserves_unaffected_runs_and_undo_restores_everything() {
        let mut e = plain("abcdef");
        Arc::make_mut(&mut e.document).paragraphs[0].runs = vec![
            Run {
                text: "abc".into(),
                font: Font::default(),
                source: None,
            },
            Run {
                text: "def".into(),
                font: Font {
                    italic: true,
                    ..Font::default()
                },
                source: None,
            },
        ];
        let before = e.document.paragraphs.clone();
        e.set_position(
            Position {
                paragraph: 0,
                offset: 2,
            },
            false,
        );
        e.set_position(
            Position {
                paragraph: 0,
                offset: 4,
            },
            true,
        );
        apply(&mut e, Edit::Insert("XYZ".into()));
        assert_eq!(e.document.paragraphs[0].text(), "abXYZef");
        assert!(!e.document.paragraphs[0].runs[0].font.italic);
        assert!(e.document.paragraphs[0].runs.last().unwrap().font.italic);
        apply(&mut e, Edit::Undo);
        assert_eq!(e.document.paragraphs, before);
        apply(&mut e, Edit::Redo);
        assert_eq!(e.document.paragraphs[0].text(), "abXYZef");
    }
    #[test]
    fn pending_change_is_atomic_and_rejects_stale_selection() {
        let mut e = editor();
        let before = e.document.paragraphs.clone();
        let pending = e.prepare(Edit::Insert("new".into())).unwrap().unwrap();
        assert_eq!(e.document.paragraphs, before);
        e.set_position(e.end(), false);
        assert!(e.commit(pending).is_err());
        assert_eq!(e.document.paragraphs, before);
        assert!(e.prepare(Edit::Insert("\0".into())).is_err());
        assert!(
            e.prepare(Edit::Insert("\n".repeat(MAX_PARAGRAPHS + 1)))
                .is_err()
        );
        assert!(
            e.prepare(Edit::Insert("a".repeat(MAX_PARAGRAPH + 1)))
                .is_err()
        );
        assert_eq!(e.document.paragraphs, before);
    }
    #[test]
    fn undo_original_clears_dirty_and_new_edit_discards_redo() {
        let mut e = editor();
        apply(&mut e, Edit::Insert("X".into()));
        assert!(e.dirty());
        apply(&mut e, Edit::Undo);
        assert!(!e.dirty());
        apply(&mut e, Edit::Insert("Y".into()));
        assert!(e.prepare(Edit::Redo).unwrap().is_none());
    }
    #[test]
    fn selection_crossing_paragraphs_replaces_exact_range() {
        let mut e = plain("one\ntwo\nthree");
        e.set_position(
            Position {
                paragraph: 0,
                offset: 2,
            },
            false,
        );
        e.set_position(
            Position {
                paragraph: 2,
                offset: 2,
            },
            true,
        );
        assert_eq!(e.selected_text(), "e\r\ntwo\r\nth");
        apply(&mut e, Edit::Insert("X".into()));
        assert_eq!(e.document.paragraphs.len(), 1);
        assert_eq!(e.document.paragraphs[0].text(), "onXree");
    }
    #[test]
    fn numbered_paragraphs_continue_and_undo_restores_labels() {
        let mut e = plain("first");
        Arc::make_mut(&mut e.document).paragraphs[0].list = Some(crate::ListSpec {
            id: "1".into(),
            level: 0,
            start: 1,
            pattern: "%1.".into(),
            decimal: true,
        });
        Arc::make_mut(&mut e.document).paragraphs[0].list_label = Some("1.".into());
        apply(&mut e, Edit::Insert("\nsecond".into()));
        assert_eq!(e.document.paragraphs[1].list_label.as_deref(), Some("2."));
        apply(&mut e, Edit::Undo);
        assert_eq!(e.document.paragraphs.len(), 1);
        assert_eq!(e.document.paragraphs[0].list_label.as_deref(), Some("1."));
    }
}
