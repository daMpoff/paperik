use paperik_document::{Alignment, Document, Font, LineSpacing, Paragraph};
use std::collections::BTreeSet;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Direct2D::{ID2D1HwndRenderTarget, ID2D1SolidColorBrush};
use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{HSTRING, Result, w};

#[derive(Clone)]
pub struct Line {
    pub layout: IDWriteTextLayout,
    pub x: f32,
    pub y: f32,
    pub source_y: f32,
    pub width: f32,
    pub height: f32,
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

pub fn color(rgb: [u8; 3]) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: rgb[0] as f32 / 255.0,
        g: rgb[1] as f32 / 255.0,
        b: rgb[2] as f32 / 255.0,
        a: 1.0,
    }
}
fn brush(target: &ID2D1HwndRenderTarget, rgb: [u8; 3]) -> Result<ID2D1SolidColorBrush> {
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
            let baseline = f32::from(metrics.ascent) * scale;
            if chosen.is_none_or(|(previous, _)| height > previous) {
                chosen = Some((height, baseline));
            }
        }
        Ok(chosen)
    }
}

fn shaped(
    context: (&IDWriteFactory, &ID2D1HwndRenderTarget),
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
        let layout = factory.CreateGdiCompatibleTextLayout(
            text,
            &format,
            width.max(1.0),
            1_000_000.0,
            1.0,
            None,
            true,
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
        let baseline = natural_baseline + (height - natural_height).max(0.0) / 2.0;
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

fn prepare(
    factory: &IDWriteFactory,
    target: &ID2D1HwndRenderTarget,
    paragraph: &Paragraph,
    width: f32,
    tab_stop: f32,
) -> Result<Prepared> {
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
    }
    let width = (width - paragraph.style.left - paragraph.style.right).max(1.0);
    let first_width = (width - paragraph.style.first_line).max(1.0);
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
            for metric in metrics(&rest)? {
                lines.push(Line {
                    layout: rest.clone(),
                    x: paragraph.style.left,
                    y: 0.0,
                    source_y: y,
                    width,
                    height: metric.height,
                });
                y += metric.height;
            }
        }
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
    target: &ID2D1HwndRenderTarget,
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
        .map(|p| prepare(factory, target, p, content_width, document.tab_stop))
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
            if paragraph.style.widow && remaining.len() > 2 && count < remaining.len() {
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
                document.tab_stop,
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
