//! Versioned, bounded session checkpoints. DOCX export is deliberately not required.
use super::*;
use std::io::{Read, Write};
const MAGIC: &[u8; 16] = b"PAPERIK-SESSION\x01";
const MAX_DECODED: usize = 128 * 1024 * 1024;
pub const MAX_CHECKPOINT: usize = 64 * 1024 * 1024;

struct Encoder(Vec<u8>);
impl Encoder {
    fn u32(&mut self, value: u32) {
        self.0.extend(value.to_le_bytes());
    }
    fn u64(&mut self, value: u64) {
        self.0.extend(value.to_le_bytes());
    }
    fn bytes(&mut self, value: &[u8]) -> Result<(), String> {
        if self.0.len().saturating_add(value.len()).saturating_add(4) > MAX_DECODED {
            return Err("Сессия превышает предел 128 МиБ".into());
        }
        self.u32(value.len() as u32);
        self.0.extend(value);
        Ok(())
    }
    fn string(&mut self, value: &str) -> Result<(), String> {
        self.bytes(value.as_bytes())
    }
    fn font(&mut self, font: &Font) -> Result<(), String> {
        self.string(&font.family)?;
        self.u32(font.points.to_bits());
        self.u32(
            u32::from(font.bold)
                | (u32::from(font.italic) << 1)
                | (u32::from(font.underline) << 2)
                | (u32::from(font.strike) << 3),
        );
        self.u32(font.kerning_from.map_or(u32::MAX, f32::to_bits));
        self.u32(u32::from(font.ligatures));
        self.0.extend(font.color);
        Ok(())
    }
    fn selection(&mut self, selection: Selection) {
        for p in [selection.anchor, selection.focus] {
            self.u32(p.paragraph as u32);
            self.u32(p.offset);
        }
    }
    fn frame(
        &mut self,
        document: &Document,
        selection: Selection,
        revision: u64,
    ) -> Result<(), String> {
        self.u64(revision);
        self.selection(selection);
        self.u32(document.paragraphs.len() as u32);
        for p in &document.paragraphs {
            self.u32(p.source.map_or(u32::MAX, |s| s as u32));
            self.u32(u32::from(p.style.page_break));
            self.u32(p.runs.len() as u32);
            for run in &p.runs {
                let (p, r) = run
                    .source
                    .map_or((u32::MAX, u32::MAX), |(p, r)| (p as u32, r as u32));
                self.u32(p);
                self.u32(r);
                self.font(&run.font)?;
                self.string(&run.text)?;
            }
        }
        if self.0.len() > MAX_DECODED {
            return Err("Сессия превышает предел 128 МиБ".into());
        }
        Ok(())
    }
}
struct Decoder<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Decoder<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self
            .position
            .checked_add(length)
            .ok_or("Некорректный размер сессии")?;
        let result = self
            .bytes
            .get(self.position..end)
            .ok_or("Снимок сессии оборван")?;
        self.position = end;
        Ok(result)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn bytes(&mut self, limit: usize) -> Result<&'a [u8], String> {
        let length = self.u32()? as usize;
        if length > limit {
            return Err("Превышен лимит данных сессии".into());
        }
        self.take(length)
    }
    fn string(&mut self, limit: usize) -> Result<String, String> {
        String::from_utf8(self.bytes(limit)?.to_vec())
            .map_err(|_| "Повреждён UTF-8 в сессии".into())
    }
    fn font(&mut self) -> Result<Font, String> {
        let family = self.string(4096)?;
        let points = f32::from_bits(self.u32()?);
        let flags = self.u32()?;
        let kern = self.u32()?;
        let kerning_from = if kern == u32::MAX {
            None
        } else {
            Some(f32::from_bits(kern))
        };
        let ligatures = self.u32()?;
        let color = self.take(3)?.try_into().unwrap();
        if family.is_empty()
            || family.contains('\0')
            || !(1.0..=400.0).contains(&points)
            || flags > 15
            || ligatures > 15
            || kerning_from.is_some_and(|v| !v.is_finite() || !(0.0..=10000.0).contains(&v))
        {
            return Err("Некорректное оформление в сессии".into());
        }
        Ok(Font {
            family,
            points,
            bold: flags & 1 != 0,
            italic: flags & 2 != 0,
            underline: flags & 4 != 0,
            strike: flags & 8 != 0,
            kerning_from,
            ligatures: ligatures as u8,
            color,
        })
    }
    fn selection(&mut self) -> Result<Selection, String> {
        let mut position = || -> Result<Position, String> {
            Ok(Position {
                paragraph: self.u32()? as usize,
                offset: self.u32()?,
            })
        };
        Ok(Selection {
            anchor: position()?,
            focus: position()?,
        })
    }
    fn frame(&mut self, baseline: &Document) -> Result<Snapshot, String> {
        let revision = self.u64()?;
        let selection = self.selection()?;
        let count = self.u32()? as usize;
        if !(1..=MAX_PARAGRAPHS).contains(&count) {
            return Err("Некорректное число абзацев сессии".into());
        }
        let mut paragraphs = Vec::with_capacity(count);
        let mut total = 0usize;
        let mut runs_total = 0usize;
        for _ in 0..count {
            let source = self.u32()?;
            let page_break = self.u32()?;
            let runs = self.u32()? as usize;
            runs_total = runs_total
                .checked_add(runs)
                .ok_or("Слишком много фрагментов")?;
            if page_break > 1 || runs_total > 500_000 {
                return Err("Некорректная структура сессии".into());
            }
            let mut paragraph = if source == u32::MAX {
                baseline
                    .paragraphs
                    .first()
                    .filter(|p| p.source.is_none())
                    .cloned()
                    .ok_or("Не найдена основа нового абзаца")?
            } else {
                baseline
                    .paragraphs
                    .get(source as usize)
                    .filter(|p| p.source == Some(source as usize))
                    .cloned()
                    .ok_or("Не найдена основа абзаца")?
            };
            paragraph.style.page_break = page_break != 0;
            paragraph.runs.clear();
            let mut length = 0usize;
            for _ in 0..runs {
                let p = self.u32()?;
                let r = self.u32()?;
                let font = self.font()?;
                let text = self.string(MAX_PARAGRAPH)?;
                length = length
                    .checked_add(text.len())
                    .ok_or("Слишком большой абзац")?;
                if length > MAX_PARAGRAPH
                    || text.chars().any(|c| {
                        (c.is_control() && c != '\r' && c != '\n' && c != '\t')
                            || matches!(c, '\u{fffe}' | '\u{ffff}')
                    })
                {
                    return Err("Некорректный текст сессии".into());
                }
                let source = if p == u32::MAX && r == u32::MAX {
                    None
                } else {
                    let old = baseline.paragraphs.get(p as usize).and_then(|p| {
                        p.runs.iter().find(|run| {
                            run.source == Some((p.source.unwrap_or(usize::MAX), r as usize))
                        })
                    });
                    if old.is_none_or(|old| old.font != font) {
                        return Err("Оформление сессии не соответствует исходному фрагменту".into());
                    }
                    Some((p as usize, r as usize))
                };
                paragraph.runs.push(Run { text, font, source });
            }
            total = total
                .checked_add(length)
                .ok_or("Слишком большой документ")?;
            if total > MAX_TEXT {
                return Err("Текст сессии превышает 16 МиБ".into());
            }
            paragraphs.push(paragraph);
        }
        let mut document = baseline.clone();
        document.paragraphs = paragraphs;
        renumber(&mut document);
        for position in [selection.anchor, selection.focus] {
            let p = document
                .paragraphs
                .get(position.paragraph)
                .ok_or("Курсор за пределами документа")?;
            let text = p.text();
            if position.offset > utf16_len(&text) || snap(&text, position.offset) != position.offset
            {
                return Err("Курсор не совпадает с границей символа".into());
            }
        }
        validate(&document)?;
        Ok(Snapshot {
            document,
            selection,
            revision,
        })
    }
}
impl Editor {
    /// Includes immutable source, current state, saved revision and bounded Undo/Redo.
    pub fn checkpoint(&self, name: &str) -> Result<Vec<u8>, String> {
        if self
            .undo
            .iter()
            .chain(&self.redo)
            .any(|s| s.document.package.original != self.document.package.original)
        {
            return Err("История сессии относится к другому исходному пакету".into());
        }
        if name.is_empty()
            || name.len() > 4096
            || name
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
        {
            return Err("Некорректное имя сессии".into());
        }
        let mut out = Encoder(Vec::new());
        out.string(name)?;
        out.bytes(&self.document.package.original)?;
        out.u64(self.saved_revision);
        out.u64(self.next_revision);
        out.frame(&self.document, self.selection, self.revision)?;
        for history in [&self.undo, &self.redo] {
            out.u32(history.len() as u32);
            for snapshot in history {
                out.frame(&snapshot.document, snapshot.selection, snapshot.revision)?;
            }
        }
        let mut compressor =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        compressor.write_all(&out.0).map_err(|e| e.to_string())?;
        let payload = compressor.finish().map_err(|e| e.to_string())?;
        let mut result = MAGIC.to_vec();
        result.extend((out.0.len() as u32).to_le_bytes());
        result.extend(crate::package::crc32(&out.0).to_le_bytes());
        result.extend((payload.len() as u32).to_le_bytes());
        result.extend(payload);
        if result.len() > MAX_CHECKPOINT {
            return Err("Сжатый снимок превышает 64 МиБ".into());
        }
        let (check, _) = Self::from_checkpoint(&result)?;
        let same = |a: &Document, b: &Document| {
            a.paragraphs == b.paragraphs
                && a.footer == b.footer
                && a.page == b.page
                && a.tab_stop == b.tab_stop
                && a.compress_justification == b.compress_justification
        };
        if !same(&check.document, &self.document)
            || check.selection != self.selection
            || [&check.undo, &check.redo]
                .into_iter()
                .zip([&self.undo, &self.redo])
                .any(|(a, b)| {
                    a.len() != b.len()
                        || a.iter().zip(b).any(|(a, b)| {
                            a.revision != b.revision
                                || a.selection != b.selection
                                || !same(&a.document, &b.document)
                        })
                })
        {
            return Err(
                "Проверка сессии не пройдена: состояние не поддержано форматом восстановления"
                    .into(),
            );
        }
        Ok(result)
    }
    pub fn from_checkpoint(bytes: &[u8]) -> Result<(Self, String), String> {
        if bytes.len() < 28
            || bytes.len() > MAX_CHECKPOINT
            || bytes.get(..16) != Some(MAGIC.as_slice())
        {
            return Err("Неизвестная версия или повреждённый формат сессии".into());
        }
        let expected = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
        let crc = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        let compressed = u32::from_le_bytes(bytes[24..28].try_into().unwrap()) as usize;
        if compressed != bytes.len() - 28 {
            return Err("Сжатый снимок оборван или содержит лишние данные".into());
        }
        if expected > MAX_DECODED {
            return Err("Снимок превышает предел распаковки".into());
        }
        let mut payload = Vec::new();
        let mut decompressor = flate2::read::DeflateDecoder::new(&bytes[28..]);
        (&mut decompressor)
            .take((MAX_DECODED + 1) as u64)
            .read_to_end(&mut payload)
            .map_err(|e| e.to_string())?;
        if payload.len() != expected
            || crate::package::crc32(&payload) != crc
            || decompressor.total_in() as usize != compressed
        {
            return Err("Снимок оборван или контрольная сумма не совпала".into());
        }
        let mut reader = Decoder {
            bytes: &payload,
            position: 0,
        };
        let name = reader.string(4096)?;
        if name.is_empty()
            || name
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
        {
            return Err("Некорректное имя сессии".into());
        }
        let baseline = Editor::new(Document::from_bytes(
            reader.bytes(32 * 1024 * 1024)?.to_vec(),
        )?)
        .document;
        let saved_revision = reader.u64()?;
        let next_revision = reader.u64()?;
        let current = reader.frame(&baseline)?;
        let mut history_bytes = 0usize;
        let mut read_history = || -> Result<VecDeque<Snapshot>, String> {
            let count = reader.u32()? as usize;
            if count > MAX_HISTORY {
                return Err("История сессии слишком велика".into());
            }
            let mut history = VecDeque::new();
            for _ in 0..count {
                let snapshot = reader.frame(&baseline)?;
                history_bytes = history_bytes
                    .checked_add(snapshot_bytes(&snapshot))
                    .ok_or("История слишком велика")?;
                if history_bytes > HISTORY_BYTES {
                    return Err("История превышает предел памяти".into());
                }
                history.push_back(snapshot);
            }
            Ok(history)
        };
        let undo = read_history()?;
        let redo = read_history()?;
        let largest = undo
            .iter()
            .chain(&redo)
            .map(|s| s.revision)
            .chain([saved_revision, current.revision])
            .max()
            .unwrap_or(0);
        if reader.position != payload.len()
            || next_revision <= largest
            || undo.len() + redo.len() > MAX_HISTORY
            || undo.iter().chain(&redo).map(snapshot_bytes).sum::<usize>() > HISTORY_BYTES
        {
            return Err("Некорректная история или лишние данные сессии".into());
        }
        Ok((
            Self {
                document: current.document,
                selection: current.selection,
                revision: current.revision,
                saved_revision,
                next_revision,
                generation: 0,
                undo,
                redo,
            },
            name,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn editor() -> Editor {
        Editor::new(
            Document::from_bytes(
                include_bytes!("../../../../tests/fixtures/formatting.docx").to_vec(),
            )
            .unwrap(),
        )
    }
    fn apply(e: &mut Editor, edit: Edit) {
        if let Some(change) = e.prepare(edit).unwrap() {
            e.commit(change).unwrap();
        }
    }
    #[test]
    fn checkpoint_restores_text_style_selection_history_and_original_package() {
        let mut e = editor();
        let original = e.document.package.original.clone();
        apply(&mut e, Edit::Insert("Я🚀\nвторая строка".into()));
        e.mark_saved();
        apply(&mut e, Edit::Insert(" draft".into()));
        apply(&mut e, Edit::Undo);
        let bytes = e.checkpoint("Пример.docx").unwrap();
        let (mut restored, name) = Editor::from_checkpoint(&bytes).unwrap();
        assert_eq!(name, "Пример.docx");
        assert_eq!(restored.document.paragraphs, e.document.paragraphs);
        assert_eq!(restored.selection, e.selection);
        assert!(!restored.dirty());
        assert_eq!(&*original, &*restored.document.package.original);
        apply(&mut restored, Edit::Redo);
        assert!(restored.dirty());
        assert!(restored.document.paragraphs[1].text().contains(" draft"));
        apply(&mut restored, Edit::Undo);
        apply(&mut restored, Edit::Undo);
        assert!(restored.dirty());
        assert_eq!(restored.document.paragraphs, editor().document.paragraphs);
    }
    #[test]
    fn session_can_restore_an_edit_that_docx_export_refuses() {
        let mut e = Editor::new(
            Document::from_bytes(crate::tests::archive(&crate::tests::fixture_parts(), false))
                .unwrap(),
        );
        e.set_position(
            Position {
                paragraph: 0,
                offset: utf16_len(&e.document.paragraphs[0].text()),
            },
            false,
        );
        apply(&mut e, Edit::Delete { word: false });
        assert!(e.document.to_docx(None).is_err());
        let (mut restored, _) =
            Editor::from_checkpoint(&e.checkpoint("Draft.docx").unwrap()).unwrap();
        assert_eq!(restored.document.paragraphs, e.document.paragraphs);
        assert!(restored.dirty());
        apply(&mut restored, Edit::Undo);
        assert!(restored.document.to_docx(None).is_ok());
    }
    #[test]
    fn truncated_corrupt_unknown_and_oversized_checkpoints_are_rejected() {
        let mut e = editor();
        apply(&mut e, Edit::Insert("draft".into()));
        let bytes = e.checkpoint("Draft.docx").unwrap();
        for length in [0, 16, 23, bytes.len() / 2, bytes.len() - 1] {
            assert!(Editor::from_checkpoint(&bytes[..length]).is_err());
        }
        let mut corrupt = bytes.clone();
        corrupt[20] ^= 1;
        assert!(Editor::from_checkpoint(&corrupt).is_err());
        let mut unknown = bytes.clone();
        unknown[15] = 2;
        assert!(Editor::from_checkpoint(&unknown).is_err());
        let mut oversized = bytes;
        oversized[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Editor::from_checkpoint(&oversized).is_err());
        assert!(e.checkpoint("../not-a-name").is_err());
    }
    #[test]
    fn embedded_docx_numbering_overflow_returns_an_error_instead_of_panicking() {
        let e = editor();
        let source = e
            .document
            .package
            .text("word/numbering.xml")
            .unwrap()
            .replace("w:val=\"1\"", "w:val=\"4294967295\"");
        let original = e
            .document
            .package
            .replace(&BTreeMap::from([(
                "word/numbering.xml".into(),
                source.into_bytes(),
            )]))
            .unwrap();
        assert!(
            Document::from_bytes(original)
                .unwrap_err()
                .contains("Нумерация")
        );
    }
}
