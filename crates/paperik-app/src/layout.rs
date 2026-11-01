use paperik_document::{Alignment, Document, Font, LineSpacing, Paragraph};
use std::collections::BTreeSet;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Direct2D::{ID2D1RenderTarget, ID2D1SolidColorBrush};
use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{HSTRING, Interface, Result, w};
mod input;
pub use input::Caret;

#[derive(Clone)]
pub struct Line {
    pub layout: IDWriteTextLayout,
    pub x: f32,
    pub y: f32,
    pub source_y: f32,
    pub width: f32,
    pub height: f32,
    pub text_start: u32,
    pub text_length: u32,
    pub paragraph: usize,
    pub baseline: f32,
    pub layout_text_start: u32,
}
pub struct PageLayout {
    pub lines: Vec<Line>,
    pub footer: Vec<Line>,
}
pub struct Layout {
    pub pages: Vec<PageLayout>,
    pub missing_fonts: BTreeSet<String>,
}
struct Prepared {
    lines: Vec<Line>,
    before: f32,
    after: f32,
}
#[derive(Clone, Copy)]
struct ParagraphSettings {
    tab_stop: f32,
    index: usize,
    compress: bool,
}

impl Layout {
    pub fn glyph_report(&self, document: &Document) -> Result<String> {
        use std::fmt::Write;
        let paragraphs: Vec<Vec<u16>> = document
            .paragraphs
            .iter()
            .map(|p| p.text().encode_utf16().collect())
            .collect();
        let mut report = String::from(
            "# Glyph positions; units=DIP; offsets=UTF-16; no source text\npage\tparagraph\toffset\tx\tbaseline\n",
        );
        for (page_index, page) in self.pages.iter().enumerate() {
            for line in &page.lines {
                let text = &paragraphs[line.paragraph];
                let prefix = if line.text_start == 0 {
                    document.paragraphs[line.paragraph]
                        .list_label
                        .as_ref()
                        .map_or(0, |s| s.encode_utf16().count() as u32 + 1)
                } else {
                    0
                };
                for (offset, unit) in text
                    .iter()
                    .enumerate()
                    .take(((line.text_start + line.text_length) as usize).min(text.len()))
                    .skip(line.text_start as usize)
                {
                    if matches!(*unit, 9 | 10 | 13 | 32 | 0xdc00..=0xdfff) {
                        continue;
                    }
                    let mut x = 0.0;
                    let mut y = 0.0;
                    let mut hit = DWRITE_HIT_TEST_METRICS::default();
                    unsafe {
                        line.layout.HitTestTextPosition(
                            line.layout_text_start + prefix + offset as u32 - line.text_start,
                            false,
                            &mut x,
                            &mut y,
                            &mut hit,
                        )?;
                    }
                    writeln!(
                        &mut report,
                        "{}\t{}\t{offset}\t{:.5}\t{:.5}",
                        page_index + 1,
                        line.paragraph + 1,
                        line.x + x,
                        line.y + line.baseline
                    )
                    .expect("Writing to a String cannot fail");
                }
            }
        }
        Ok(report)
    }
    /// Physical geometry only: diagnostics never include document text.
    pub fn report(&self, document: &Document) -> Result<String> {
        use std::fmt::Write;
        let mut report = String::from(
            "# Paperik layout; units=DIP; offsets=UTF-16; page/paragraph=1-based\npage\tparagraph\tstart\tlength\tx\tbaseline\theight\twidth\n",
        );
        for (index, page) in self.pages.iter().enumerate() {
            for line in &page.lines {
                let prefix = if line.text_start == 0 {
                    document.paragraphs[line.paragraph]
                        .list_label
                        .as_ref()
                        .map_or(0, |s| s.encode_utf16().count() as u32 + 1)
                } else {
                    0
                };
                let mut x = 0.0;
                let mut y = 0.0;
                let mut hit = DWRITE_HIT_TEST_METRICS::default();
                unsafe {
                    line.layout.HitTestTextPosition(
                        line.layout_text_start + prefix,
                        false,
                        &mut x,
                        &mut y,
                        &mut hit,
                    )?;
                }
                writeln!(
                    &mut report,
                    "{}\t{}\t{}\t{}\t{:.5}\t{:.5}\t{:.5}\t{:.5}",
                    index + 1,
                    line.paragraph + 1,
                    line.text_start,
                    line.text_length,
                    line.x + x,
                    line.y + line.baseline,
                    line.height,
                    line.width
                )
                .expect("Writing to a String cannot fail");
            }
        }
        Ok(report)
    }
}

pub fn color(rgb: [u8; 3]) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: rgb[0] as f32 / 255.0,
        g: rgb[1] as f32 / 255.0,
        b: rgb[2] as f32 / 255.0,
        a: 1.0,
    }
}
fn brush(target: &ID2D1RenderTarget, rgb: [u8; 3]) -> Result<ID2D1SolidColorBrush> {
    // SAFETY: the color is valid and the returned COM object owns its resources.
    unsafe { target.CreateSolidColorBrush(&color(rgb), None) }
}

fn metrics(layout: &IDWriteTextLayout) -> Result<Vec<DWRITE_LINE_METRICS>> {
    // SAFETY: the first call obtains the required length; the second owns that buffer.
    unsafe {
        let mut count = 0;
        let _ = layout.GetLineMetrics(None, &mut count);
        let mut lines = vec![DWRITE_LINE_METRICS::default(); count as usize];
        layout.GetLineMetrics(Some(&mut lines), &mut count)?;
        lines.truncate(count as usize);
        Ok(lines)
    }
}

fn font_line_box(factory: &IDWriteFactory, paragraph: &Paragraph) -> Result<Option<(f32, f32)>> {
    // Physical document spacing must use font design units rather than the
    // integer pixel rounding in GDI-compatible glyph placement.
    unsafe {
        let mut collection = None;
        factory.GetSystemFontCollection(&mut collection, false)?;
        let Some(collection) = collection else {
            return Ok(None);
        };
        let mut chosen = None;
        for font in fonts(paragraph) {
            let mut index = 0;
            let mut exists = Default::default();
            collection.FindFamilyName(&HSTRING::from(&font.family), &mut index, &mut exists)?;
            if !exists.as_bool() {
                continue;
            }
            let actual = collection.GetFontFamily(index)?.GetFirstMatchingFont(
                if font.bold {
                    DWRITE_FONT_WEIGHT_BOLD
                } else {
                    DWRITE_FONT_WEIGHT_NORMAL
                },
                DWRITE_FONT_STRETCH_NORMAL,
                if font.italic {
                    DWRITE_FONT_STYLE_ITALIC
                } else {
                    DWRITE_FONT_STYLE_NORMAL
                },
            )?;
            let mut metrics = DWRITE_FONT_METRICS::default();
            actual.GetMetrics(&mut metrics);
            if metrics.designUnitsPerEm == 0 {
                continue;
            }
            let scale = font.points * 4.0 / 3.0 / f32::from(metrics.designUnitsPerEm);
            let height = (f32::from(metrics.ascent)
                + f32::from(metrics.descent)
                + f32::from(metrics.lineGap))
                * scale;
            let baseline = (f32::from(metrics.ascent) + f32::from(metrics.lineGap)) * scale;
            if chosen.is_none_or(|(previous, _)| height > previous) {
                chosen = Some((height, baseline));
            }
        }
        Ok(chosen)
    }
}

fn shaped(
    context: (&IDWriteFactory, &ID2D1RenderTarget),
    paragraph: &Paragraph,
    text: &[u16],
    start: u32,
    prefix: u32,
    width: f32,
    tab_stop: f32,
) -> Result<IDWriteTextLayout> {
    let (factory, target) = context;
    let base = paragraph
        .runs
        .first()
        .map(|r| &r.font)
        .unwrap_or(&paragraph.mark_font);
    // SAFETY: HSTRING parameters and slice buffers live through synchronous COM calls.
    unsafe {
        let format = factory.CreateTextFormat(
            &HSTRING::from(&base.family),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            base.points * 4.0 / 3.0,
            w!("ru-RU"),
        )?;
        format.SetIncrementalTabStop(tab_stop.max(1.0))?;
        let layout = factory.CreateTextLayout(text, &format, width.max(1.0), 1_000_000.0)?;
        let advanced: IDWriteTextLayout1 = layout.cast()?;
        typography(
            factory,
            &layout,
            base,
            DWRITE_TEXT_RANGE {
                startPosition: 0,
                length: text.len() as u32,
            },
        )?;
        let mut position = prefix;
        for run in &paragraph.runs {
            let end = position + run.text.encode_utf16().count() as u32;
            let lo = position.max(start);
            let hi = end.min(start + text.len() as u32);
            if hi > lo {
                let range = DWRITE_TEXT_RANGE {
                    startPosition: lo - start,
                    length: hi - lo,
                };
                layout.SetFontFamilyName(&HSTRING::from(&run.font.family), range)?;
                layout.SetFontSize(run.font.points * 4.0 / 3.0, range)?;
                layout.SetFontWeight(
                    if run.font.bold {
                        DWRITE_FONT_WEIGHT_BOLD
                    } else {
                        DWRITE_FONT_WEIGHT_NORMAL
                    },
                    range,
                )?;
                layout.SetFontStyle(
                    if run.font.italic {
                        DWRITE_FONT_STYLE_ITALIC
                    } else {
                        DWRITE_FONT_STYLE_NORMAL
                    },
                    range,
                )?;
                layout.SetUnderline(run.font.underline, range)?;
                layout.SetStrikethrough(run.font.strike, range)?;
                advanced.SetPairKerning(
                    run.font
                        .kerning_from
                        .is_some_and(|v| v > 0.0 && run.font.points >= v),
                    range,
                )?;
                typography(factory, &layout, &run.font, range)?;
                layout.SetDrawingEffect(&brush(target, run.font.color)?, range)?;
            }
            position = end;
        }
        let natural = metrics(&layout)?;
        let max_points = paragraph
            .runs
            .iter()
            .map(|r| r.font.points)
            .fold(paragraph.mark_font.points, f32::max);
        let reference = natural.first().copied().unwrap_or_default();
        let (natural_height, natural_baseline) = font_line_box(factory, paragraph)?.unwrap_or((
            reference.height.max(max_points * 4.0 / 3.0),
            reference.baseline,
        ));
        let height = match paragraph.style.line {
            LineSpacing::Multiple(v) => natural_height * v.max(0.1),
            LineSpacing::Exact(v) => v.max(1.0),
            LineSpacing::AtLeast(v) => natural_height.max(v),
        };
        let height = (height * 15.0).round() / 15.0;
        let baseline = natural_baseline
            + match paragraph.style.line {
                LineSpacing::Multiple(_) => 0.0,
                _ => (height - natural_height).max(0.0) / 2.0,
            };
        layout.SetLineSpacing(
            DWRITE_LINE_SPACING_METHOD_UNIFORM,
            height,
            baseline.min(height),
        )?;
        layout.SetTextAlignment(match paragraph.style.alignment {
            Alignment::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
            Alignment::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
            Alignment::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            Alignment::Justify => DWRITE_TEXT_ALIGNMENT_JUSTIFIED,
        })?;
        Ok(layout)
    }
}

fn typography(
    factory: &IDWriteFactory,
    layout: &IDWriteTextLayout,
    font: &Font,
    range: DWRITE_TEXT_RANGE,
) -> Result<()> {
    unsafe {
        let typography = factory.CreateTypography()?;
        for (name, parameter) in [
            (
                DWRITE_FONT_FEATURE_TAG_KERNING,
                u32::from(
                    font.kerning_from
                        .is_some_and(|v| v > 0.0 && font.points >= v),
                ),
            ),
            (
                DWRITE_FONT_FEATURE_TAG_STANDARD_LIGATURES,
                u32::from(font.ligatures & 1 != 0),
            ),
            (
                DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_LIGATURES,
                u32::from(font.ligatures & 2 != 0),
            ),
            (
                DWRITE_FONT_FEATURE_TAG_DISCRETIONARY_LIGATURES,
                u32::from(font.ligatures & 4 != 0),
            ),
            (
                DWRITE_FONT_FEATURE_TAG_HISTORICAL_LIGATURES,
                u32::from(font.ligatures & 8 != 0),
            ),
        ] {
            typography.AddFontFeature(DWRITE_FONT_FEATURE {
                nameTag: name,
                parameter,
            })?;
        }
        layout.SetTypography(&typography, range)?;
    }
    Ok(())
}

fn prepare(
    factory: &IDWriteFactory,
    target: &ID2D1RenderTarget,
    paragraph: &Paragraph,
    width: f32,
    settings: ParagraphSettings,
) -> Result<Prepared> {
    let ParagraphSettings {
        tab_stop,
        index: paragraph_index,
        ..
    } = settings;
    let prefix = paragraph
        .list_label
        .as_ref()
        .map(|label| format!("{label}\t"))
        .unwrap_or_default();
    let mut text: Vec<u16> = prefix
        .encode_utf16()
        .chain(paragraph.runs.iter().flat_map(|r| r.text.encode_utf16()))
        .collect();
    if text.is_empty() {
        text.push(32)
    } else if matches!(text.last(), Some(10 | 13)) {
        // A final soft break owns a visible empty line and a caret position.
        // The virtual space is never part of the document or exported text.
        text.push(32)
    }
    let width = (width - paragraph.style.left - paragraph.style.right).max(1.0);
    let first_width = (width - paragraph.style.first_line).max(1.0);
    if paragraph.style.alignment == Alignment::Justify {
        return justified(
            (factory, target),
            paragraph,
            &text,
            prefix.encode_utf16().count() as u32,
            (first_width, width),
            settings,
        );
    }
    let first = shaped(
        (factory, target),
        paragraph,
        &text,
        0,
        prefix.encode_utf16().count() as u32,
        first_width,
        tab_stop,
    )?;
    let first_metrics = metrics(&first)?;
    let mut lines = Vec::new();
    if let Some(line) = first_metrics.first() {
        lines.push(Line {
            layout: first.clone(),
            x: paragraph.style.left + paragraph.style.first_line,
            y: 0.0,
            source_y: 0.0,
            width: first_width,
            height: line.height,
            text_start: 0,
            text_length: line
                .length
                .saturating_sub(prefix.encode_utf16().count() as u32),
            paragraph: paragraph_index,
            baseline: line.baseline,
            layout_text_start: 0,
        });
        let first_length = line.length.min(text.len() as u32) as usize;
        if first_length < text.len() {
            let rest = shaped(
                (factory, target),
                paragraph,
                &text[first_length..],
                first_length as u32,
                prefix.encode_utf16().count() as u32,
                width,
                tab_stop,
            )?;
            let mut y = 0.0;
            let mut position = first_length as u32;
            for metric in metrics(&rest)? {
                lines.push(Line {
                    layout: rest.clone(),
                    x: paragraph.style.left,
                    y: 0.0,
                    source_y: y,
                    width,
                    height: metric.height,
                    text_start: position.saturating_sub(prefix.encode_utf16().count() as u32),
                    text_length: metric.length,
                    paragraph: paragraph_index,
                    baseline: metric.baseline,
                    layout_text_start: position - first_length as u32,
                });
                y += metric.height;
                position += metric.length;
            }
        }
    }
    Ok(Prepared {
        lines,
        before: paragraph.style.before,
        after: paragraph.style.after,
    })
}

// Justification may shrink ordinary inter-word spaces. Scaling the whole line
// would also distort glyphs; only the advance of eligible U+0020 clusters moves.
const MAX_SPACE_SHRINK: f32 = 0.25;

fn justified(
    context: (&IDWriteFactory, &ID2D1RenderTarget),
    paragraph: &Paragraph,
    text: &[u16],
    prefix: u32,
    widths: (f32, f32),
    settings: ParagraphSettings,
) -> Result<Prepared> {
    let ParagraphSettings {
        tab_stop,
        index: paragraph_index,
        compress,
    } = settings;
    let measured = shaped(context, paragraph, text, 0, prefix, 1_000_000.0, tab_stop)?;
    let mut count = 0;
    unsafe {
        let _ = measured.GetClusterMetrics(None, &mut count);
    }
    let mut native = vec![DWRITE_CLUSTER_METRICS::default(); count as usize];
    unsafe {
        measured.GetClusterMetrics(Some(&mut native), &mut count)?;
    }
    native.truncate(count as usize);
    let mut position = 0_usize;
    let clusters: Vec<_> = native
        .iter()
        .map(|c| {
            let start = position;
            position += usize::from(c.length);
            (start, position, *c)
        })
        .collect();
    let mut lines = Vec::new();
    let mut begin = 0;
    while begin < clusters.len() {
        let width = if begin == 0 { widths.0 } else { widths.1 };
        let mut advance = 0.0;
        let mut spaces = 0.0;
        let mut visible_advance = 0.0;
        let mut visible_spaces = 0.0;
        let mut selected = None;
        let mut best_adjustment = f32::INFINITY;
        for (j, (a, b, c)) in clusters.iter().enumerate().skip(begin) {
            advance += c.width;
            if *b - *a == 1 && text[*a] == 32 && *a >= prefix as usize {
                spaces += c.width;
            }
            if c._bitfield & 2 == 0 {
                visible_advance = advance;
                visible_spaces = spaces;
            }
            let fits = visible_advance
                - visible_spaces * if compress { MAX_SPACE_SHRINK } else { 0.0 }
                <= width + 0.001;
            let hard_break = c._bitfield & 4 != 0;
            // Keep a solidus expression together (e.g. a price and its /unit).
            let wraps = c._bitfield & 1 != 0 && text[b - 1] != u16::from(b'/');
            if fits && (wraps || j + 1 == clusters.len() || hard_break) {
                let last = j + 1 == clusters.len() || hard_break;
                let adjustment = if last && visible_advance <= width {
                    0.0
                } else if visible_spaces > 0.0 {
                    ((width - visible_advance) / visible_spaces).abs()
                } else {
                    f32::INFINITY
                };
                if selected.is_none() || adjustment <= best_adjustment {
                    selected = Some(j + 1);
                    best_adjustment = adjustment;
                }
            }
            if hard_break || !fits {
                break;
            }
        }
        // For a word wider than the column, retain the native cluster boundary.
        let end = selected.unwrap_or_else(|| {
            let mut total = 0.0;
            let mut end = begin;
            for (_, _, c) in &clusters[begin..] {
                if total + c.width > width && end > begin {
                    break;
                }
                total += c.width;
                end += 1;
            }
            end.max(begin + 1)
        });
        let start = clusters[begin].0;
        let stop = clusters[end - 1].1;
        let fragment = &text[start..stop];
        let layout = shaped(
            context,
            paragraph,
            fragment,
            start as u32,
            prefix,
            1_000_000.0,
            tab_stop,
        )?;
        unsafe {
            layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
        }
        let mut measured_width = DWRITE_TEXT_METRICS::default();
        unsafe {
            layout.GetMetrics(&mut measured_width)?;
        }
        let last_nonspace = fragment
            .iter()
            .rposition(|c| !matches!(*c, 32 | 9 | 10 | 13))
            .map_or(0, |i| i + 1);
        let eligible: Vec<_> = clusters[begin..end]
            .iter()
            .filter(|(a, b, _)| {
                *b - *a == 1
                    && text[*a] == 32
                    && *a >= prefix as usize
                    && *a < start + last_nonspace
            })
            .collect();
        let space_width: f32 = eligible.iter().map(|(_, _, c)| c.width).sum();
        let hard_break = clusters[end - 1].2._bitfield & 4 != 0;
        let should_justify = measured_width.width > width || (end < clusters.len() && !hard_break);
        if should_justify && space_width > 0.0 {
            let ratio = ((width - measured_width.width) / space_width).max(-MAX_SPACE_SHRINK);
            let advanced: IDWriteTextLayout1 = layout.cast()?;
            for (a, _, c) in eligible {
                unsafe {
                    advanced.SetCharacterSpacing(
                        0.0,
                        c.width * ratio,
                        c.width * (1.0 - MAX_SPACE_SHRINK),
                        DWRITE_TEXT_RANGE {
                            startPosition: (a - start) as u32,
                            length: 1,
                        },
                    )?;
                }
            }
        }
        let metric = metrics(&layout)?[0];
        lines.push(Line {
            layout,
            x: paragraph.style.left
                + if begin == 0 {
                    paragraph.style.first_line
                } else {
                    0.0
                },
            y: 0.0,
            source_y: 0.0,
            width,
            height: metric.height,
            text_start: (start as u32).saturating_sub(prefix),
            text_length: (stop as u32).saturating_sub(prefix)
                - (start as u32).saturating_sub(prefix),
            paragraph: paragraph_index,
            baseline: metric.baseline,
            layout_text_start: 0,
        });
        begin = end;
    }
    Ok(Prepared {
        lines,
        before: paragraph.style.before,
        after: paragraph.style.after,
    })
}

fn fonts(paragraph: &Paragraph) -> impl Iterator<Item = &Font> {
    std::iter::once(&paragraph.mark_font).chain(paragraph.runs.iter().map(|r| &r.font))
}

pub fn build(
    factory: &IDWriteFactory,
    target: &ID2D1RenderTarget,
    document: &Document,
) -> Result<Layout> {
    let mut missing_fonts = BTreeSet::new();
    // SAFETY: COM returns a retained font collection; the output buffer is local.
    unsafe {
        let mut collection = None;
        factory.GetSystemFontCollection(&mut collection, false)?;
        if let Some(collection) = collection {
            for font in document
                .paragraphs
                .iter()
                .chain(document.footer.iter())
                .flat_map(fonts)
            {
                let mut index = 0;
                let mut exists = Default::default();
                collection.FindFamilyName(&HSTRING::from(&font.family), &mut index, &mut exists)?;
                if !exists.as_bool() {
                    missing_fonts.insert(font.family.clone());
                }
            }
        }
    }
    let content_width = document.page.width - document.page.left - document.page.right;
    let paragraphs_prepared: Vec<_> = document
        .paragraphs
        .iter()
        .enumerate()
        .map(|(i, p)| {
            prepare(
                factory,
                target,
                p,
                content_width,
                ParagraphSettings {
                    tab_stop: document.tab_stop,
                    index: i,
                    compress: document.compress_justification,
                },
            )
        })
        .collect::<Result<_>>()?;
    let mut pages = vec![PageLayout {
        lines: Vec::new(),
        footer: Vec::new(),
    }];
    let top = document.page.top;
    let bottom = document.page.height - document.page.bottom;
    let mut y = top;
    let mut previous_after = 0.0_f32;
    for (index, paragraph) in document.paragraphs.iter().enumerate() {
        let prepared = &paragraphs_prepared[index];
        let mut before = prepared.before.max(previous_after);
        if index > 0
            && paragraph.style.contextual
            && document.paragraphs[index - 1].style_id == paragraph.style_id
        {
            before = 0.0;
        }
        let height: f32 = prepared.lines.iter().map(|l| l.height).sum();
        let next_height = if paragraph.style.keep_next {
            paragraphs_prepared
                .get(index + 1)
                .map(|p| p.before + p.lines.first().map(|l| l.height).unwrap_or(0.0))
                .unwrap_or(0.0)
        } else {
            0.0
        };
        if y > top
            && (paragraph.style.page_break
                || ((paragraph.style.keep_lines || paragraph.style.keep_next)
                    && height + before + next_height > bottom - y
                    && height + next_height <= bottom - top))
        {
            pages.push(PageLayout {
                lines: Vec::new(),
                footer: Vec::new(),
            });
            y = top;
            before = 0.0;
        }
        if y > top {
            y += before;
        }
        let mut remaining = prepared.lines.as_slice();
        while !remaining.is_empty() {
            let mut count = 0;
            let mut used = 0.0;
            for line in remaining {
                if y + used + line.height > bottom + 0.01 {
                    break;
                }
                used += line.height;
                count += 1;
            }
            if paragraph.style.widow && remaining.len() >= 2 && count < remaining.len() {
                if count == 1 && y > top {
                    count = 0;
                } else if remaining.len() - count == 1 && count > 2 {
                    count -= 1;
                }
            }
            if count == 0 {
                if y <= top {
                    count = 1;
                } else {
                    pages.push(PageLayout {
                        lines: Vec::new(),
                        footer: Vec::new(),
                    });
                    y = top;
                    continue;
                }
            }
            for source in &remaining[..count] {
                let mut line = source.clone();
                line.x += document.page.left;
                line.y = y;
                y += line.height;
                pages.last_mut().unwrap().lines.push(line);
            }
            remaining = &remaining[count..];
            if !remaining.is_empty() {
                pages.push(PageLayout {
                    lines: Vec::new(),
                    footer: Vec::new(),
                });
                y = top;
            }
            if pages.len() > 2000 {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                    0x8007000e_u32 as i32,
                )));
            }
        }
        previous_after = prepared.after;
    }
    for (index, page) in pages.iter_mut().enumerate() {
        if index == 0 && document.page.different_first {
            continue;
        }
        let mut footer_lines = Vec::new();
        for original in &document.footer {
            let mut paragraph = original.clone();
            if paragraph.page_field
                && let Some(run) = paragraph
                    .runs
                    .iter_mut()
                    .find(|r| r.text.trim().parse::<usize>().is_ok())
            {
                run.text = (index + 1).to_string();
            }
            let prepared = prepare(
                factory,
                target,
                &paragraph,
                content_width,
                ParagraphSettings {
                    tab_stop: document.tab_stop,
                    index: usize::MAX,
                    compress: document.compress_justification,
                },
            )?;
            footer_lines.extend(prepared.lines);
        }
        let total: f32 = footer_lines.iter().map(|l| l.height).sum();
        let mut footer_y = document.page.height - document.page.footer_distance - total;
        for mut line in footer_lines {
            line.x += document.page.left;
            line.y = footer_y;
            footer_y += line.height;
            page.footer.push(line);
        }
    }
    Ok(Layout {
        pages,
        missing_fonts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use paperik_document::{ParagraphStyle, Run};
    use windows::Win32::Graphics::Direct2D::Common::*;
    use windows::Win32::Graphics::Direct2D::*;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    fn context() -> (IDWriteFactory, ID2D1DCRenderTarget) {
        unsafe {
            let factory: ID2D1Factory =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap();
            let target = factory
                .CreateDCRenderTarget(&D2D1_RENDER_TARGET_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_IGNORE,
                    },
                    ..Default::default()
                })
                .unwrap();
            (
                DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap(),
                target,
            )
        }
    }
    fn paragraph(text: &str) -> Paragraph {
        let font = Font {
            points: 14.0,
            ..Default::default()
        };
        Paragraph {
            runs: vec![Run {
                text: text.into(),
                font: font.clone(),
            }],
            mark_font: font,
            style: ParagraphStyle {
                first_line: 0.0,
                ..Default::default()
            },
            style_id: String::new(),
            list_label: None,
            list: None,
            page_field: false,
        }
    }
    #[test]
    fn modern_justification_fits_by_spaces_without_scaling_glyphs() {
        let (factory, target) = context();
        let mut paragraph = paragraph("alpha beta gamma");
        paragraph.style.alignment = Alignment::Justify;
        let text: Vec<_> = paragraph.text().encode_utf16().collect();
        let raw = shaped((&factory, &target), &paragraph, &text, 0, 0, 1000.0, 48.0).unwrap();
        let mut measured = DWRITE_TEXT_METRICS::default();
        unsafe {
            raw.GetMetrics(&mut measured).unwrap();
        }
        let width = measured.width - 0.5;
        let legacy = prepare(
            &factory,
            &target,
            &paragraph,
            width,
            ParagraphSettings {
                tab_stop: 48.0,
                index: 0,
                compress: false,
            },
        )
        .unwrap();
        let modern = prepare(
            &factory,
            &target,
            &paragraph,
            width,
            ParagraphSettings {
                tab_stop: 48.0,
                index: 0,
                compress: true,
            },
        )
        .unwrap();
        assert!(legacy.lines.len() > 1);
        assert_eq!(modern.lines.len(), 1);
        let hit = |layout: &IDWriteTextLayout, position| {
            let mut x = 0.0;
            let mut y = 0.0;
            let mut info = DWRITE_HIT_TEST_METRICS::default();
            unsafe {
                layout
                    .HitTestTextPosition(position, false, &mut x, &mut y, &mut info)
                    .unwrap();
            }
            x
        };
        assert!(
            ((hit(&raw, 1) - hit(&raw, 0))
                - (hit(&modern.lines[0].layout, 1) - hit(&modern.lines[0].layout, 0)))
            .abs()
                < 0.001
        );
        assert_eq!(paragraph.runs[0].font.points, 14.0);
    }
    #[test]
    fn proportional_spacing_keeps_the_first_baseline() {
        let (factory, target) = context();
        let mut paragraph = paragraph("Sample");
        let text: Vec<_> = paragraph.text().encode_utf16().collect();
        let single = shaped((&factory, &target), &paragraph, &text, 0, 0, 500.0, 48.0).unwrap();
        paragraph.style.line = LineSpacing::Multiple(1.5);
        let spaced = shaped((&factory, &target), &paragraph, &text, 0, 0, 500.0, 48.0).unwrap();
        let a = metrics(&single).unwrap()[0];
        let b = metrics(&spaced).unwrap()[0];
        assert!((a.baseline - b.baseline).abs() < 0.001);
        assert!((b.height - a.height * 1.5).abs() < 1.0 / 15.0);
    }
    #[test]
    fn widow_control_keeps_a_two_line_paragraph_on_one_page() {
        let (factory, target) = context();
        let mut doc = Document::from_bytes(
            include_bytes!("../../../tests/fixtures/formatting.docx").to_vec(),
        )
        .unwrap();
        doc.paragraphs = vec![paragraph("first"), paragraph("one\ntwo")];
        doc.footer.clear();
        doc.page.width = 400.0;
        doc.page.height = 40.0;
        doc.page.left = 0.0;
        doc.page.right = 0.0;
        doc.page.top = 0.0;
        doc.page.bottom = 0.0;
        for p in &mut doc.paragraphs {
            p.style.line = LineSpacing::Exact(20.0);
        }
        let kept = build(&factory, &target, &doc).unwrap();
        assert_eq!(
            kept.pages.iter().map(|p| p.lines.len()).collect::<Vec<_>>(),
            vec![1, 2]
        );
        doc.paragraphs[1].style.widow = false;
        let split = build(&factory, &target, &doc).unwrap();
        assert_eq!(
            split
                .pages
                .iter()
                .map(|p| p.lines.len())
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
    }
    #[test]
    fn a_price_and_its_solidus_unit_are_not_split() {
        let (factory, target) = context();
        let mut paragraph = paragraph("AAA 50/GB BBB");
        paragraph.style.alignment = Alignment::Justify;
        let prefix: Vec<_> = "AAA 50/".encode_utf16().collect();
        let raw = shaped((&factory, &target), &paragraph, &prefix, 0, 0, 1000.0, 48.0).unwrap();
        let mut size = DWRITE_TEXT_METRICS::default();
        unsafe {
            raw.GetMetrics(&mut size).unwrap();
        }
        let result = prepare(
            &factory,
            &target,
            &paragraph,
            size.width + 0.1,
            ParagraphSettings {
                tab_stop: 48.0,
                index: 0,
                compress: true,
            },
        )
        .unwrap();
        assert_eq!(result.lines[0].text_length, 4);
    }
    #[test]
    fn target_dpi_does_not_change_document_geometry() {
        let (factory, target) = context();
        let doc = Document::from_bytes(
            include_bytes!("../../../tests/fixtures/formatting.docx").to_vec(),
        )
        .unwrap();
        let before = build(&factory, &target, &doc)
            .unwrap()
            .report(&doc)
            .unwrap();
        unsafe {
            target.SetDpi(192.0, 192.0);
        }
        let after = build(&factory, &target, &doc)
            .unwrap()
            .report(&doc)
            .unwrap();
        assert_eq!(before, after);
    }
    #[test]
    fn caret_hit_testing_and_highlight_follow_native_shaping() {
        use paperik_document::editor::{Position, Selection, utf16_len};
        let (factory, target) = context();
        let mut doc = Document::from_bytes(
            include_bytes!("../../../tests/fixtures/formatting.docx").to_vec(),
        )
        .unwrap();
        doc.paragraphs = vec![paragraph("abc Я🚀z"), paragraph("")];
        doc.footer.clear();
        let layout = build(&factory, &target, &doc).unwrap();
        for offset in [0, 1, 2, 3, 4, 5, 7, 8] {
            let position = Position {
                paragraph: 0,
                offset,
            };
            let caret = layout.caret(&doc, position, false).unwrap().unwrap();
            let (hit, _) = layout
                .hit_test(
                    &doc,
                    caret.page,
                    caret.x + 0.01,
                    caret.y + caret.height / 2.0,
                )
                .unwrap()
                .unwrap();
            assert_eq!(position, hit, "offset {offset}");
        }
        let empty = Position {
            paragraph: 1,
            offset: 0,
        };
        let caret = layout.caret(&doc, empty, false).unwrap().unwrap();
        assert_eq!(
            layout
                .hit_test(&doc, caret.page, caret.x + 500.0, caret.y + 1.0)
                .unwrap()
                .unwrap()
                .0,
            empty
        );
        let selection = Selection {
            anchor: Position {
                paragraph: 0,
                offset: 4,
            },
            focus: Position {
                paragraph: 0,
                offset: utf16_len("abc Я🚀z"),
            },
        };
        let highlights = layout.highlights(&doc, 0, selection).unwrap();
        assert!(!highlights.is_empty());
        assert!(highlights.iter().all(|r| r.width > 0.0 && r.height > 0.0));
    }
    #[test]
    fn soft_break_and_wrapped_line_have_distinct_caret_positions() {
        use paperik_document::editor::{Position, utf16_len};
        let (factory, target) = context();
        let mut doc = Document::from_bytes(
            include_bytes!("../../../tests/fixtures/formatting.docx").to_vec(),
        )
        .unwrap();
        for alignment in [Alignment::Left, Alignment::Justify] {
            let mut p = paragraph("word word word word word word\n");
            p.style.alignment = alignment;
            doc.paragraphs = vec![p];
            doc.footer.clear();
            doc.page.width = 350.0;
            doc.page.left = 96.0;
            doc.page.right = 96.0;
            let layout = build(&factory, &target, &doc).unwrap();
            let lines = &layout.pages[0].lines;
            assert!(lines.len() >= 3);
            let boundary = Position {
                paragraph: 0,
                offset: lines[1].text_start,
            };
            let up = layout.caret(&doc, boundary, true).unwrap().unwrap();
            let down = layout.caret(&doc, boundary, false).unwrap().unwrap();
            assert!(up.y < down.y);
            let end = Position {
                paragraph: 0,
                offset: utf16_len(&doc.paragraphs[0].text()),
            };
            let caret = layout.caret(&doc, end, false).unwrap().unwrap();
            assert_eq!(caret.line, lines.len() - 1);
            assert_eq!(
                layout
                    .hit_test(&doc, 0, caret.x, caret.y + 1.0)
                    .unwrap()
                    .unwrap()
                    .0,
                end
            );
        }
    }
    #[test]
    fn editing_grows_pages_and_undo_restores_identical_geometry() {
        use paperik_document::editor::{Edit, Editor};
        let (factory, target) = context();
        let doc = Document::from_bytes(
            include_bytes!("../../../tests/fixtures/formatting.docx").to_vec(),
        )
        .unwrap();
        let original = build(&factory, &target, &doc).unwrap();
        let before = original.report(&doc).unwrap();
        let fonts: Vec<_> = doc
            .paragraphs
            .iter()
            .flat_map(|p| p.runs.iter().map(|r| r.font.clone()))
            .collect();
        let mut editor = Editor::new(doc);
        editor.set_position(editor.end(), false);
        let change = editor
            .prepare(Edit::Insert(
                "\nAdditional paragraph for pagination.".repeat(100),
            ))
            .unwrap()
            .unwrap();
        let grown = build(&factory, &target, &change.document).unwrap();
        assert!(grown.pages.len() > original.pages.len());
        editor.commit(change).unwrap();
        let change = editor.prepare(Edit::Undo).unwrap().unwrap();
        let restored = build(&factory, &target, &change.document).unwrap();
        editor.commit(change).unwrap();
        assert_eq!(before, restored.report(editor.document()).unwrap());
        assert_eq!(
            fonts,
            editor
                .document()
                .paragraphs
                .iter()
                .flat_map(|p| p.runs.iter().map(|r| r.font.clone()))
                .collect::<Vec<_>>()
        );
        assert!(!editor.dirty());
    }
}
