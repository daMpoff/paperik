//! Hit testing uses the very same shaped lines that are drawn on the page.
use super::{Layout, Line};
use paperik_document::{
    Document,
    editor::{Position, Selection, snap, utf16_len},
};
use windows::{Win32::Graphics::DirectWrite::DWRITE_HIT_TEST_METRICS, core::Result};

#[derive(Clone, Copy, Debug)]
pub struct Caret {
    pub page: usize,
    pub line: usize,
    pub x: f32,
    pub y: f32,
    pub height: f32,
}
pub struct Highlight {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
fn prefix(document: &Document, line: &Line) -> u32 {
    if line.text_start == 0 {
        document.paragraphs[line.paragraph]
            .list_label
            .as_ref()
            .map_or(0, |s| utf16_len(s) + 1)
    } else {
        0
    }
}
fn native(document: &Document, line: &Line, offset: u32) -> u32 {
    line.layout_text_start + prefix(document, line) + offset.saturating_sub(line.text_start)
}
fn end(document: &Document, line: &Line) -> u32 {
    (line.text_start + line.text_length).min(utf16_len(&document.paragraphs[line.paragraph].text()))
}
fn hit_line(document: &Document, line: &Line, x: f32) -> Result<Position> {
    let mut trailing = Default::default();
    let mut inside = Default::default();
    let mut hit = DWRITE_HIT_TEST_METRICS::default();
    unsafe {
        line.layout.HitTestPoint(
            x - line.x,
            line.source_y + line.baseline * 0.7,
            &mut trailing,
            &mut inside,
            &mut hit,
        )?;
    }
    let local = hit.textPosition + if trailing.as_bool() { hit.length } else { 0 };
    let offset =
        line.text_start + local.saturating_sub(line.layout_text_start + prefix(document, line));
    let text = document.paragraphs[line.paragraph].text();
    let units: Vec<_> = text.encode_utf16().collect();
    let mut limit = end(document, line);
    while limit > line.text_start && matches!(units[(limit - 1) as usize], 10 | 13) {
        limit -= 1;
    }
    Ok(Position {
        paragraph: line.paragraph,
        offset: snap(&text, offset.clamp(line.text_start, limit)),
    })
}
impl Layout {
    pub fn hit_test(
        &self,
        document: &Document,
        page: usize,
        x: f32,
        y: f32,
    ) -> Result<Option<(Position, bool)>> {
        let Some(page) = self.pages.get(page) else {
            return Ok(None);
        };
        let Some(line) = page.lines.iter().min_by(|a, b| {
            let distance = |line: &Line| (line.y - y).max(0.0).max(y - line.y - line.height);
            distance(a).total_cmp(&distance(b))
        }) else {
            return Ok(None);
        };
        let position = hit_line(document, line, x)?;
        Ok(Some((position, position.offset == end(document, line))))
    }
    pub fn caret(
        &self,
        document: &Document,
        position: Position,
        upstream: bool,
    ) -> Result<Option<Caret>> {
        let candidates: Vec<_> = self
            .pages
            .iter()
            .enumerate()
            .flat_map(|(p, page)| {
                page.lines
                    .iter()
                    .enumerate()
                    .map(move |(l, line)| (p, l, line))
            })
            .filter(|(_, _, line)| {
                line.paragraph == position.paragraph
                    && position.offset >= line.text_start
                    && position.offset <= end(document, line)
            })
            .collect();
        let choice = if upstream {
            candidates.first()
        } else {
            candidates.last()
        };
        let Some(&(page, index, line)) = choice else {
            return Ok(None);
        };
        let offset = native(document, line, position.offset);
        let at_end = position.offset == end(document, line) && position.offset > line.text_start;
        let mut x = 0.0;
        let mut y = 0.0;
        let mut hit = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            line.layout.HitTestTextPosition(
                if at_end {
                    offset.saturating_sub(1)
                } else {
                    offset
                },
                at_end,
                &mut x,
                &mut y,
                &mut hit,
            )?;
        }
        Ok(Some(Caret {
            page,
            line: index,
            x: line.x + x,
            y: line.y,
            height: line.height.max(1.0),
        }))
    }
    pub fn vertical(
        &self,
        document: &Document,
        position: Position,
        upstream: bool,
        x: f32,
        steps: isize,
    ) -> Result<Option<(Position, bool)>> {
        let Some(caret) = self.caret(document, position, upstream)? else {
            return Ok(None);
        };
        let lines: Vec<_> = self
            .pages
            .iter()
            .enumerate()
            .flat_map(|(p, page)| {
                page.lines
                    .iter()
                    .enumerate()
                    .map(move |(l, line)| (p, l, line))
            })
            .collect();
        let current = lines
            .iter()
            .position(|(p, l, _)| *p == caret.page && *l == caret.line)
            .unwrap_or(0);
        let index = current
            .saturating_add_signed(steps)
            .min(lines.len().saturating_sub(1));
        let Some((_, _, line)) = lines.get(index) else {
            return Ok(None);
        };
        let position = hit_line(document, line, x)?;
        Ok(Some((position, position.offset == end(document, line))))
    }
    pub fn line_edge(
        &self,
        document: &Document,
        position: Position,
        upstream: bool,
        right: bool,
    ) -> Result<Option<Position>> {
        let Some(caret) = self.caret(document, position, upstream)? else {
            return Ok(None);
        };
        let line = &self.pages[caret.page].lines[caret.line];
        let mut offset = if right {
            end(document, line)
        } else {
            line.text_start
        };
        if right {
            let text: Vec<u16> = document.paragraphs[line.paragraph]
                .text()
                .encode_utf16()
                .collect();
            while offset > line.text_start && matches!(text[(offset - 1) as usize], 10 | 13) {
                offset -= 1;
            }
        }
        Ok(Some(Position {
            paragraph: line.paragraph,
            offset,
        }))
    }
    pub fn highlights(
        &self,
        document: &Document,
        page: usize,
        selection: Selection,
    ) -> Result<Vec<Highlight>> {
        let mut result = Vec::new();
        if selection.collapsed() {
            return Ok(result);
        }
        let (start, stop) = selection.range();
        for line in &self.pages[page].lines {
            if line.paragraph < start.paragraph || line.paragraph > stop.paragraph {
                continue;
            }
            let a = line.text_start.max(if line.paragraph == start.paragraph {
                start.offset
            } else {
                0
            });
            let b = end(document, line).min(if line.paragraph == stop.paragraph {
                stop.offset
            } else {
                u32::MAX
            });
            if a < b {
                let mut count = 0;
                // DirectWrite reports the required count with an insufficient-buffer HRESULT.
                unsafe {
                    let _ = line.layout.HitTestTextRange(
                        native(document, line, a),
                        b - a,
                        line.x,
                        line.y - line.source_y,
                        None,
                        &mut count,
                    );
                }
                if count > 200_000 {
                    return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                        0x8007000e_u32 as i32,
                    )));
                }
                let mut metrics = vec![DWRITE_HIT_TEST_METRICS::default(); count as usize];
                unsafe {
                    line.layout.HitTestTextRange(
                        native(document, line, a),
                        b - a,
                        line.x,
                        line.y - line.source_y,
                        Some(&mut metrics),
                        &mut count,
                    )?;
                }
                for hit in metrics.into_iter().take(count as usize) {
                    let top = hit.top.max(line.y);
                    let bottom = (hit.top + hit.height).min(line.y + line.height);
                    if bottom > top {
                        result.push(Highlight {
                            x: hit.left,
                            y: top,
                            width: hit.width.max(1.0),
                            height: bottom - top,
                        });
                    }
                }
            }
            // Paragraph separators are selectable even when the paragraph is empty.
            if line.paragraph < stop.paragraph
                && end(document, line) == utf16_len(&document.paragraphs[line.paragraph].text())
            {
                let p = Position {
                    paragraph: line.paragraph,
                    offset: end(document, line),
                };
                if let Some(caret) = self.caret(document, p, true)? {
                    result.push(Highlight {
                        x: caret.x,
                        y: line.y,
                        width: 5.0,
                        height: line.height,
                    });
                }
            }
        }
        Ok(result)
    }
}
