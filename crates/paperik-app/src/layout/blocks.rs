//! Block flow for tables and inline pictures; shaping is shared with the ordinary editor.
use super::*;
use paperik_document::{Block, Border, Table, TableCell, VerticalMerge};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone)]
pub struct CellLayout {
    pub rect: [f32; 4],
    pub fill: Option<[u8; 3]>,
    pub row_height: f32,
    pub borders: [Option<Border>; 4],
}
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}
pub struct ImageLayout {
    pub paragraph: usize,
    pub offset: Option<u32>,
    pub rect: [f32; 4],
    pub pixels: Arc<Pixels>,
}
pub(super) fn error(message: &str) -> windows::core::Error {
    windows::core::Error::new(windows::core::HRESULT(0x80004001_u32 as i32), message)
}
pub(super) fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(windows::core::Error::from_hresult(windows::core::HRESULT(
            0x80004004_u32 as i32,
        )))
    } else {
        Ok(())
    }
}
use windows::Win32::Graphics::Imaging::*;

fn with_frame<T>(
    bytes: &[u8],
    read: impl FnOnce(&IWICImagingFactory, &IWICBitmapFrameDecode, u32, u32) -> Result<T>,
) -> Result<T> {
    use windows::Win32::System::Com::*;
    struct Com(bool);
    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                unsafe {
                    CoUninitialize();
                }
            }
        }
    }
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_MULTITHREADED);
        if initialized.is_err() && initialized.0 != 0x80010106_u32 as i32 {
            initialized.ok()?;
        }
        let _com = Com(initialized.is_ok());
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let stream = factory.CreateStream()?;
        stream.InitializeFromMemory(bytes)?;
        let decoder = factory.CreateDecoderFromStream(
            &stream,
            std::ptr::null(),
            WICDecodeMetadataCacheOnLoad,
        )?;
        if decoder.GetFrameCount()? != 1 {
            return Err(error("Многокадровые рисунки пока не поддерживаются"));
        }
        let frame = decoder.GetFrame(0)?;
        let (mut width, mut height) = (0, 0);
        frame.GetSize(&mut width, &mut height)?;
        if width == 0
            || height == 0
            || width > 16384
            || height > 16384
            || u64::from(width) * u64::from(height) > 16_000_000
        {
            return Err(error("Рисунок превышает предел 16 миллионов пикселей"));
        }
        read(&factory, &frame, width, height)
    }
}

pub(super) fn picture_dimensions(bytes: &[u8]) -> Result<[u32; 2]> {
    with_frame(bytes, |_, _, width, height| Ok([width, height]))
}

pub(super) fn decode(bytes: &[u8], budget: usize) -> Result<Pixels> {
    with_frame(bytes, |factory, frame, width, height| unsafe {
        if u64::from(width) * u64::from(height) * 4 > budget as u64 {
            return Err(error(
                "Рисунки превышают предел 64 МиБ декодированных данных",
            ));
        }
        let converter = factory.CreateFormatConverter()?;
        converter.Initialize(
            frame,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let mut pixels = vec![0; width as usize * height as usize * 4];
        converter.CopyPixels(std::ptr::null(), width * 4, &mut pixels)?;
        Ok(Pixels {
            width,
            height,
            bytes: pixels,
        })
    })
}

#[derive(Clone)]
struct Row {
    cells: Vec<CellLayout>,
    lines: Vec<Line>,
    height: f32,
}
struct CellDraft<'a> {
    source: &'a TableCell,
    x: f32,
    width: f32,
    height: f32,
    lines: Vec<Line>,
}

fn prepare_cell<'a>(
    factory: &IDWriteFactory,
    doc: &Document,
    table: &Table,
    cell: &'a TableCell,
    cancel: &AtomicBool,
) -> Result<CellDraft<'a>> {
    let x: f32 = table.columns[..cell.column].iter().sum();
    let width: f32 = table.columns[cell.column..cell.column + cell.span]
        .iter()
        .sum();
    let [top, right, bottom, left] = cell.margins;
    let inner = width - left - right;
    if inner < 1.0 {
        return Err(error("Поля ячейки не оставляют места для текста"));
    }
    let mut lines = Vec::new();
    let mut y = top;
    let mut previous = 0.0_f32;
    for index in &cell.paragraphs {
        check(cancel)?;
        let paragraph = &doc.paragraphs[*index];
        let prepared = prepare(
            factory,
            paragraph,
            inner,
            ParagraphSettings {
                tab_stop: doc.tab_stop,
                index: *index,
                compress: doc.compress_justification,
            },
        )?;
        y += previous.max(prepared.before);
        for mut line in prepared.lines {
            line.x += x + left;
            line.y = y;
            y += line.height;
            lines.push(line);
        }
        previous = prepared.after;
    }
    Ok(CellDraft {
        source: cell,
        x,
        width,
        height: y + previous + bottom,
        lines,
    })
}

fn group_end(table: &Table, first: usize) -> usize {
    let mut end = first + 1;
    while table.rows.get(end).is_some_and(|row| {
        row.cells
            .iter()
            .any(|cell| cell.merge == Some(VerticalMerge::Continue))
    }) {
        end += 1;
    }
    end
}

fn row_group(
    factory: &IDWriteFactory,
    doc: &Document,
    table: &Table,
    first: usize,
    cancel: &AtomicBool,
) -> Result<Row> {
    let rows = &table.rows[first..group_end(table, first)];
    let mut heights: Vec<_> = rows.iter().map(|r| r.min_height).collect();
    let mut drafts = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        check(cancel)?;
        for cell in &row.cells {
            if cell.merge == Some(VerticalMerge::Continue) {
                continue;
            }
            let draft = prepare_cell(factory, doc, table, cell, cancel)?;
            let mut end = index + 1;
            let mut bottom_border = cell.borders[2].clone();
            if cell.merge == Some(VerticalMerge::Start) {
                while let Some(continuation) = rows.get(end).and_then(|row| {
                    row.cells.iter().find(|c| {
                        c.column == cell.column
                            && c.span == cell.span
                            && c.merge == Some(VerticalMerge::Continue)
                    })
                }) {
                    bottom_border = continuation.borders[2].clone();
                    end += 1;
                }
            }
            if end == index + 1 {
                if row.exact_height && draft.height > heights[index] + 0.05 {
                    return Err(error("Текст не помещается в точную высоту строки таблицы"));
                }
                heights[index] = heights[index].max(draft.height);
            }
            drafts.push((index, end, draft, bottom_border));
        }
        if heights[index] == 0.0 {
            heights[index] = 8.0;
        }
    }
    for (start, end, draft, _) in &drafts {
        let available: f32 = heights[*start..*end].iter().sum();
        if draft.height > available + 0.05 {
            let row = (*start..*end)
                .rev()
                .find(|i| !rows[*i].exact_height)
                .ok_or_else(|| error("Текст не помещается в точную высоту объединённой ячейки"))?;
            heights[row] += draft.height - available;
        }
    }
    let mut offsets = vec![0.0];
    for height in &heights {
        offsets.push(offsets.last().unwrap() + height);
    }
    let mut cells = Vec::new();
    let mut lines = Vec::new();
    for (start, end, draft, bottom) in drafts {
        let y = offsets[start];
        let height = offsets[end] - y;
        let [top, right, bottom_margin, left] = draft.source.margins;
        let free = (height - draft.height).max(0.0);
        let dy = match draft.source.vertical {
            Alignment::Center => free / 2.0,
            Alignment::Right => free,
            _ => 0.0,
        };
        let mut borders = draft.source.borders.clone();
        borders[2] = bottom;
        cells.push(CellLayout {
            rect: [draft.x, y, draft.width, height],
            fill: draft.source.fill,
            borders,
            row_height: heights[start],
        });
        for mut line in draft.lines {
            line.y += y + dy;
            line.clip = Some([
                draft.x + left,
                y + top,
                draft.width - right - left,
                height - top - bottom_margin,
            ]);
            lines.push(line);
        }
    }
    Ok(Row {
        cells,
        lines,
        height: *offsets.last().unwrap(),
    })
}
fn add_row(page: &mut PageLayout, row: &Row, x: f32, y: f32) {
    for cell in &row.cells {
        let mut cell = cell.clone();
        cell.rect[0] += x;
        cell.rect[1] += y;
        page.cells.push(cell);
    }
    for line in &row.lines {
        let mut line = line.clone();
        line.x += x;
        line.y += y;
        if let Some(clip) = &mut line.clip {
            clip[0] += x;
            clip[1] += y;
        }
        page.lines.push(line);
    }
}
fn next_page(pages: &mut Vec<PageLayout>, y: &mut f32, top: f32) -> Result<()> {
    if pages.len() >= 2000 {
        return Err(error("Превышен предел 2000 страниц"));
    }
    pages.push(PageLayout::default());
    *y = top;
    Ok(())
}

pub(super) fn build(
    factory: &IDWriteFactory,
    doc: &Document,
    cancel: &AtomicBool,
    missing_fonts: BTreeSet<String>,
) -> Result<Layout> {
    let content = doc.content.as_ref().expect("complex document");
    let width = doc.page.width - doc.page.left - doc.page.right;
    let top = doc.page.top;
    let bottom = doc.page.height - doc.page.bottom;
    let mut y = top;
    let mut after = 0.0_f32;
    let mut pages = vec![PageLayout::default()];
    let mut decoded_bytes = 0_usize;
    for (block_index, block) in content.blocks.iter().enumerate() {
        check(cancel)?;
        match block {
            Block::Paragraph(index) => {
                let paragraph = &doc.paragraphs[*index];
                let p = prepare(
                    factory,
                    paragraph,
                    width,
                    ParagraphSettings {
                        tab_stop: doc.tab_stop,
                        index: *index,
                        compress: doc.compress_justification,
                    },
                )?;
                let height: f32 = p.lines.iter().map(|l| l.height).sum();
                let next_height = if paragraph.style.keep_next {
                    match content.blocks.get(block_index + 1) {
                        Some(Block::Table(table)) => {
                            row_group(factory, doc, table, 0, cancel)?.height
                        }
                        Some(Block::Picture(pic)) => pic.height,
                        Some(Block::Paragraph(i)) => prepare(
                            factory,
                            &doc.paragraphs[*i],
                            width,
                            ParagraphSettings {
                                tab_stop: doc.tab_stop,
                                index: *i,
                                compress: doc.compress_justification,
                            },
                        )?
                        .lines
                        .first()
                        .map_or(0.0, |l| l.height),
                        None => 0.0,
                    }
                } else {
                    0.0
                };
                let before = after.max(p.before);
                if y > top
                    && (paragraph.style.page_break
                        || ((paragraph.style.keep_next || paragraph.style.keep_lines)
                            && height + before + next_height > bottom - y
                            && height + next_height <= bottom - top))
                {
                    next_page(&mut pages, &mut y, top)?;
                }
                if y > top {
                    y += before;
                }
                let mut remaining = p.lines.as_slice();
                while !remaining.is_empty() {
                    check(cancel)?;
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
                        if y > top {
                            next_page(&mut pages, &mut y, top)?;
                            continue;
                        }
                        count = 1;
                    }
                    for line in &remaining[..count] {
                        let mut line = line.clone();
                        line.x += doc.page.left;
                        line.y = y;
                        y += line.height;
                        pages.last_mut().unwrap().lines.push(line);
                    }
                    remaining = &remaining[count..];
                    if !remaining.is_empty() {
                        next_page(&mut pages, &mut y, top)?;
                    }
                }
                after = p.after;
            }
            Block::Table(table) => {
                let table_width: f32 = table.columns.iter().sum();
                let x = doc.page.left
                    + table.indent
                    + match table.alignment {
                        Alignment::Center => (width - table_width) / 2.0,
                        Alignment::Right => width - table_width,
                        _ => 0.0,
                    };
                if table_width > width + 0.25 || x < 0.0 || x + table_width > doc.page.width + 0.25
                {
                    return Err(error(
                        "Таблица не помещается в область страницы; размеры не уменьшены",
                    ));
                }
                if y > top {
                    y += after;
                }
                after = 0.0;
                let header_count = table.rows.iter().take_while(|r| r.header).count();
                let mut headers = Vec::new();
                let mut ri = 0;
                while ri < table.rows.len() {
                    let row = row_group(factory, doc, table, ri, cancel)?;
                    if row.height > bottom - top {
                        return Err(error(
                            "Строка таблицы выше доступной страницы; разбиение внутри строки пока не поддерживается",
                        ));
                    }
                    if y + row.height > bottom + 0.01 && y > top {
                        next_page(&mut pages, &mut y, top)?;
                        if ri >= header_count {
                            let h: f32 = headers.iter().map(|r: &Row| r.height).sum();
                            if h + row.height > bottom - top {
                                return Err(error(
                                    "Заголовок и строка таблицы не помещаются на странице",
                                ));
                            }
                            for header in &headers {
                                add_row(pages.last_mut().unwrap(), header, x, y);
                                y += header.height;
                            }
                        }
                    }
                    add_row(pages.last_mut().unwrap(), &row, x, y);
                    y += row.height;
                    if ri < header_count {
                        headers.push(row);
                    }
                    ri = group_end(table, ri);
                }
            }
            Block::Picture(picture) => {
                let p = &doc.paragraphs[picture.paragraph];
                let line_box = picture.height + p.mark_font.points * 4.0 / 3.0 * 0.2;
                let before = after.max(p.style.before);
                if picture.width + p.style.left + p.style.right > width + 0.25
                    || line_box > bottom - top
                {
                    return Err(error(
                        "Рисунок не помещается на страницу; размеры не уменьшены",
                    ));
                }
                if y > top && (p.style.page_break || y + before + line_box > bottom) {
                    next_page(&mut pages, &mut y, top)?;
                }
                if y > top {
                    y += before;
                }
                let pixels = decode(&picture.bytes, 64 * 1024 * 1024 - decoded_bytes)?;
                check(cancel)?;
                decoded_bytes += pixels.bytes.len();
                if decoded_bytes > 64 * 1024 * 1024 {
                    return Err(error(
                        "Рисунки превышают предел 64 МиБ декодированных данных",
                    ));
                }
                let available = width - p.style.left - p.style.right;
                let x = doc.page.left
                    + p.style.left
                    + match p.style.alignment {
                        Alignment::Center => (available - picture.width) / 2.0,
                        Alignment::Right => available - picture.width,
                        _ => p.style.first_line,
                    };
                pages.last_mut().unwrap().images.push(ImageLayout {
                    paragraph: picture.paragraph,
                    offset: None,
                    rect: [x, y, picture.width, picture.height],
                    pixels: Arc::new(pixels),
                });
                y += line_box;
                after = p.style.after;
            }
        }
    }
    add_footers(factory, doc, &mut pages)?;
    Ok(Layout {
        pages,
        missing_fonts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Document {
        Document::from_bytes(include_bytes!("../../../../tests/fixtures/blocks.docx").to_vec())
            .unwrap()
    }
    fn layout(doc: &Document) -> Result<Layout> {
        let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }?;
        super::super::build_cancelled(&factory, doc, &AtomicBool::new(false))
    }
    #[test]
    fn tables_paginate_repeat_headers_and_keep_all_text_and_pixels() {
        let doc = fixture();
        let original = doc.paragraphs.clone();
        let result = layout(&doc).unwrap();
        assert!(result.pages.len() >= 2);
        let seen: BTreeSet<_> = result
            .pages
            .iter()
            .flat_map(|p| p.lines.iter().map(|l| l.paragraph))
            .collect();
        let expected: BTreeSet<_> = doc
            .paragraphs
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.text().is_empty())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(seen, expected);
        assert!(result.pages[1].lines.iter().any(|l| l.paragraph == 2));
        let image = &result.pages[0].images[0];
        assert_eq!(&image.rect[2..], &[96.0, 48.0]);
        assert_eq!((image.pixels.width, image.pixels.height), (32, 16));
        assert_eq!(&image.pixels.bytes[..4], &[79, 102, 40, 255]);
        assert_eq!(
            result.pages.iter().map(|p| p.images.len()).sum::<usize>(),
            1
        );
        for page in &result.pages {
            for cell in &page.cells {
                assert!(cell.rect[1] >= doc.page.top - 0.01);
                assert!(cell.rect[1] + cell.rect[3] <= doc.page.height - doc.page.bottom + 0.01);
            }
            for line in &page.lines {
                if let Some([_, y, _, h]) = line.clip {
                    assert!(line.y >= y - 0.01);
                    assert!(line.y + line.height <= y + h + 0.01);
                }
            }
        }
        assert_eq!(doc.paragraphs, original);
    }
    #[test]
    fn oversize_tables_and_corrupt_images_do_not_silently_disappear() {
        let mut doc = fixture();
        let Block::Table(table) = &mut doc.content.as_mut().unwrap().blocks[2] else {
            unreachable!()
        };
        table.columns[0] = 5000.0;
        assert!(layout(&doc).is_err());
        let mut doc = fixture();
        let Block::Picture(image) = &mut doc.content.as_mut().unwrap().blocks[1] else {
            unreachable!()
        };
        image.bytes = Arc::new(b"\x89PNG\r\n\x1a\ninvalid".to_vec());
        assert!(layout(&doc).is_err());
        let doc = fixture();
        let factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.unwrap();
        assert!(super::super::build_cancelled(&factory, &doc, &AtomicBool::new(true)).is_err());
    }

    #[test]
    fn editing_cell_text_grows_pages_without_changing_grid_fonts_or_image() {
        use paperik_document::editor::{Edit, Editor, Position};
        let original = fixture();
        let before = layout(&original).unwrap();
        let mut editor = Editor::new(original.clone());
        // Make many rows taller while keeping each row smaller than a page.
        for row in 1..25 {
            let Block::Table(table) = &editor.document().content.as_ref().unwrap().blocks[2] else {
                unreachable!()
            };
            let p = table.rows[row].cells[0].paragraphs[0];
            editor.set_position(
                Position {
                    paragraph: p,
                    offset: 0,
                },
                false,
            );
            let change = editor
                .prepare(Edit::Insert("Дополнение к содержимому ячейки ".repeat(7)))
                .unwrap()
                .unwrap();
            editor.commit(change).unwrap();
        }
        let result = layout(editor.document()).unwrap();
        assert!(result.pages.len() > before.pages.len());
        assert_eq!(editor.document().content, original.content);
        for (a, b) in editor
            .document()
            .paragraphs
            .iter()
            .zip(&original.paragraphs)
        {
            assert_eq!(a.style, b.style);
            assert!(
                a.runs
                    .iter()
                    .all(|r| b.runs.iter().any(|old| old.font == r.font))
            );
        }
        assert_eq!(
            result.pages[0].images[0].rect,
            before.pages[0].images[0].rect
        );
        let seen: BTreeSet<_> = result
            .pages
            .iter()
            .flat_map(|p| p.lines.iter().map(|l| l.paragraph))
            .collect();
        assert!(
            editor
                .document()
                .paragraphs
                .iter()
                .enumerate()
                .filter(|(_, p)| !p.text().is_empty())
                .all(|(i, _)| seen.contains(&i))
        );
    }

    #[test]
    fn mouse_columns_vertical_keys_and_repeated_header_caret_follow_visual_cells() {
        use paperik_document::editor::Position;
        let doc = fixture();
        let result = layout(&doc).unwrap();
        let page = &result.pages[0];
        for cell in &page.cells {
            let [x, y, w, h] = cell.rect;
            let hit = result
                .hit_test(&doc, 0, x + w / 2.0, y + h / 2.0)
                .unwrap()
                .unwrap()
                .0;
            let line = page
                .lines
                .iter()
                .find(|l| l.paragraph == hit.paragraph)
                .unwrap();
            let [left, top, width, height] = line.clip.unwrap();
            assert!(
                left >= x - 0.01
                    && left + width <= x + w + 0.01
                    && top >= y - 0.01
                    && top + height <= y + h + 0.01
            );
        }
        let position = Position {
            paragraph: 4,
            offset: 0,
        };
        let caret = result.caret(&doc, position, false).unwrap().unwrap();
        let down = result
            .vertical(&doc, position, false, caret.x, 1, Some(0))
            .unwrap()
            .unwrap();
        assert_eq!(down.0.paragraph, 6);
        let up = result
            .vertical(&doc, down.0, false, caret.x, -1, Some(down.2))
            .unwrap()
            .unwrap();
        assert_eq!(up.0.paragraph, 4);
        for page in 0..result.pages.len() {
            let caret = result
                .caret_on_page(
                    &doc,
                    Position {
                        paragraph: 2,
                        offset: 0,
                    },
                    false,
                    Some(page),
                )
                .unwrap()
                .unwrap();
            assert_eq!(caret.page, page);
        }
        let [x, y, w, h] = result.pages[0].images[0].rect;
        assert!(
            result
                .hit_test(&doc, 0, x + w / 2.0, y + h / 2.0)
                .unwrap()
                .is_none()
        );
    }
}
