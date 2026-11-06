//! Bounded block import for tables and standalone inline raster pictures.
use super::*;
mod edit;
use std::{collections::HashMap, sync::Arc};

#[derive(Clone, Debug, PartialEq)]
pub struct Border {
    pub width: f32,
    pub color: [u8; 3],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerticalMerge {
    Start,
    Continue,
}
#[derive(Clone, Debug, PartialEq)]
pub struct TableCell {
    pub paragraphs: Vec<usize>,
    pub column: usize,
    pub span: usize,
    pub merge: Option<VerticalMerge>,
    /// top, right, bottom, left, in DIP
    pub margins: [f32; 4],
    pub fill: Option<[u8; 3]>,
    pub borders: [Option<Border>; 4],
    pub vertical: Alignment,
}
#[derive(Clone, Debug, PartialEq)]
pub struct TableRow {
    pub cells: Vec<TableCell>,
    pub min_height: f32,
    pub exact_height: bool,
    pub header: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    pub columns: Vec<f32>,
    pub rows: Vec<TableRow>,
    pub alignment: Alignment,
    pub indent: f32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    pub bytes: Arc<Vec<u8>>,
    pub width: f32,
    pub height: f32,
    pub paragraph: usize,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Paragraph(usize),
    Table(Table),
    Picture(Picture),
}
#[derive(Clone, Debug, PartialEq)]
pub struct ViewContent {
    pub blocks: Vec<Block>,
}

fn rgb(value: &str) -> Option<[u8; 3]> {
    if value.len() != 6 || !value.is_ascii() {
        return None;
    }
    u32::from_str_radix(value, 16)
        .ok()
        .map(|v| [(v >> 16) as u8, (v >> 8) as u8, v as u8])
}
fn border(node: Node<'_, '_>) -> Result<Option<Border>, String> {
    match attr(node, "val").unwrap_or("single") {
        "nil" | "none" => Ok(None),
        "single" => Ok(Some(Border {
            width: number(attr(node, "sz")).unwrap_or(4.0) / 6.0,
            color: attr(node, "color").and_then(rgb).unwrap_or([0; 3]),
        })),
        _ => Err("Этот вид границы таблицы пока не поддерживается".into()),
    }
}
fn margins(
    properties: Option<Node<'_, '_>>,
    name: &str,
    values: &mut [f32; 4],
) -> Result<(), String> {
    if let Some(m) = properties.and_then(|n| child(n, name)) {
        for (index, tag) in ["top", "right", "bottom", "left"].iter().enumerate() {
            if let Some(n) = child(m, tag) {
                if !matches!(attr(n, "type"), None | Some("dxa")) {
                    return Err("Неподдерживаемые единицы полей ячейки".into());
                }
                values[index] = number(attr(n, "w"))
                    .filter(|v| *v >= 0.0)
                    .ok_or("Некорректные поля ячейки")?
                    / 15.0;
            }
        }
    }
    Ok(())
}
fn table(
    node: Node<'_, '_>,
    styles: &Xml<'_>,
    indices: &HashMap<roxmltree::NodeId, usize>,
) -> Result<Table, String> {
    if node
        .descendants()
        .any(|n| is(n, "tbl") && n.id() != node.id())
    {
        return Err("Вложенные таблицы пока не отображаются".into());
    }
    if node
        .descendants()
        .any(|n| is(n, "hMerge") || is(n, "gridBefore") || is(n, "gridAfter"))
    {
        return Err(
            "Старый формат объединения или неполная сетка таблицы пока не поддерживается".into(),
        );
    }
    if node
        .children()
        .any(|n| n.is_element() && !["tblPr", "tblGrid", "tr"].iter().any(|tag| is(n, tag)))
    {
        return Err("Неподдерживаемый блок внутри таблицы".into());
    }
    let props = child(node, "tblPr");
    if props.and_then(|p| child(p, "tblpPr")).is_some() {
        return Err("Плавающие таблицы пока не отображаются".into());
    }
    let columns: Vec<f32> = child(node, "tblGrid")
        .ok_or("У таблицы нет сетки колонок")?
        .children()
        .filter(|n| is(*n, "gridCol"))
        .map(|n| {
            number(attr(n, "w"))
                .filter(|v| *v > 0.0)
                .map(|v| v / 15.0)
                .ok_or("Некорректная ширина колонки таблицы")
        })
        .collect::<Result<_, _>>()?;
    if columns.is_empty() || columns.len() > 128 {
        return Err("Сетка таблицы выходит за пределы 1–128 колонок".into());
    }
    let mut chain = Vec::new();
    let mut visited = BTreeSet::new();
    let mut id = props.and_then(|p| val(p, "tblStyle")).unwrap_or("");
    while !id.is_empty() {
        if !visited.insert(id) || chain.len() >= 64 {
            return Err("Цикл стилей таблицы".into());
        }
        let style = styles
            .descendants()
            .find(|n| is(*n, "style") && attr(*n, "styleId") == Some(id))
            .ok_or("Не найден стиль таблицы")?;
        if style.children().any(|n| is(n, "tblStylePr")) {
            return Err("Условные стили таблиц пока не отображаются точно".into());
        }
        chain.push(style);
        id = val(style, "basedOn").unwrap_or("");
    }
    let mut table_margins = [0.0, 7.2, 0.0, 7.2];
    let mut borders: BTreeMap<&str, Option<Border>> = BTreeMap::new();
    for p in chain
        .into_iter()
        .rev()
        .filter_map(|n| child(n, "tblPr"))
        .chain(props)
    {
        margins(Some(p), "tblCellMar", &mut table_margins)?;
        if let Some(b) = child(p, "tblBorders") {
            for tag in ["top", "right", "bottom", "left", "insideH", "insideV"] {
                if let Some(n) = child(b, tag) {
                    borders.insert(tag, border(n)?);
                }
            }
        }
    }
    let row_nodes: Vec<_> = node.children().filter(|n| is(*n, "tr")).collect();
    if row_nodes.is_empty() || row_nodes.len() > 10000 {
        return Err("Количество строк таблицы вне пределов 1–10000".into());
    }
    let mut rows = Vec::new();
    for (ri, row) in row_nodes.iter().enumerate() {
        if row
            .children()
            .any(|n| n.is_element() && !is(n, "tc") && !is(n, "trPr"))
        {
            return Err("Неподдерживаемый блок внутри строки таблицы".into());
        }
        if child(*row, "tblPrEx").is_some() {
            return Err("Исключения свойств таблицы пока не поддерживаются".into());
        }
        let mut cells = Vec::new();
        let mut column = 0;
        for cell in row.children().filter(|n| is(*n, "tc")) {
            let cp = child(cell, "tcPr");
            let span = cp
                .and_then(|p| val(p, "gridSpan"))
                .unwrap_or("1")
                .parse::<usize>()
                .map_err(|_| "Некорректное объединение ячеек")?;
            if span == 0 || span > columns.len() - column {
                return Err("Ячейки выходят за сетку таблицы".into());
            }
            let paragraphs = cell
                .children()
                .filter(|n| is(*n, "p"))
                .map(|p| {
                    indices
                        .get(&p.id())
                        .copied()
                        .ok_or("Не найден абзац ячейки")
                })
                .collect::<Result<Vec<_>, _>>()?;
            if paragraphs.is_empty()
                || cell
                    .children()
                    .any(|n| n.is_element() && !is(n, "p") && !is(n, "tcPr"))
            {
                return Err("Ячейка содержит неподдерживаемые блоки".into());
            }
            let mut cell_margins = table_margins;
            margins(cp, "tcMar", &mut cell_margins)?;
            let tags = [
                if ri == 0 { "top" } else { "insideH" },
                if column + span == columns.len() {
                    "right"
                } else {
                    "insideV"
                },
                if ri + 1 == row_nodes.len() {
                    "bottom"
                } else {
                    "insideH"
                },
                if column == 0 { "left" } else { "insideV" },
            ];
            let mut cell_borders = std::array::from_fn(|i| borders.get(tags[i]).cloned().flatten());
            if let Some(b) = cp.and_then(|p| child(p, "tcBorders")) {
                for (i, tag) in ["top", "right", "bottom", "left"].iter().enumerate() {
                    if let Some(n) = child(b, tag) {
                        cell_borders[i] = border(n)?;
                    }
                }
            }
            let fill = cp
                .and_then(|p| child(p, "shd"))
                .and_then(|n| attr(n, "fill"))
                .and_then(rgb);
            let vertical = match cp.and_then(|p| val(p, "vAlign")).unwrap_or("top") {
                "center" => Alignment::Center,
                "bottom" => Alignment::Right,
                "top" => Alignment::Left,
                _ => return Err("Неподдерживаемое выравнивание ячейки".into()),
            };
            let merge = match cp.and_then(|p| child(p, "vMerge")) {
                None => None,
                Some(merge) => match attr(merge, "val").unwrap_or("continue") {
                    "restart" => Some(VerticalMerge::Start),
                    "continue" => Some(VerticalMerge::Continue),
                    _ => return Err("Некорректное вертикальное объединение".into()),
                },
            };
            if merge == Some(VerticalMerge::Continue)
                && cell.children().filter(|n| is(*n, "p")).any(|p| {
                    crate::save::edit_block(p).is_some()
                        || p.descendants().any(|n| {
                            is(n, "drawing")
                                || is(n, "tab")
                                || is(n, "br")
                                || is(n, "cr")
                                || is(n, "noBreakHyphen")
                                || (is(n, "t") && n.text().is_some_and(|text| !text.is_empty()))
                        })
                })
            {
                return Err("Продолжение объединения содержит скрытый текст или объекты".into());
            }
            cells.push(TableCell {
                paragraphs,
                column,
                span,
                merge,
                margins: cell_margins,
                fill,
                borders: cell_borders,
                vertical,
            });
            column += span;
        }
        if column != columns.len() {
            return Err("Количество ячеек не совпадает с сеткой таблицы".into());
        }
        let rp = child(*row, "trPr");
        let height = rp.and_then(|p| child(p, "trHeight"));
        rows.push(TableRow {
            cells,
            min_height: height.and_then(|n| number(attr(n, "val"))).unwrap_or(0.0) / 15.0,
            exact_height: height.and_then(|n| attr(n, "hRule")) == Some("exact"),
            header: rp.and_then(|p| child(p, "tblHeader")).is_some_and(enabled),
        });
    }
    for (index, row) in rows.iter().enumerate() {
        for cell in &row.cells {
            if cell.merge == Some(VerticalMerge::Continue) {
                let previous = index.checked_sub(1).and_then(|i| rows.get(i));
                if !previous.is_some_and(|r| {
                    r.header == row.header
                        && r.cells.iter().any(|c| {
                            c.column == cell.column && c.span == cell.span && c.merge.is_some()
                        })
                }) {
                    return Err(
                        "Вертикальное объединение выходит за сетку или пересекает границу шапки"
                            .into(),
                    );
                }
            }
        }
    }
    let alignment = match props.and_then(|p| val(p, "jc")).unwrap_or("left") {
        "center" => Alignment::Center,
        "right" => Alignment::Right,
        _ => Alignment::Left,
    };
    let indent = props
        .and_then(|p| child(p, "tblInd"))
        .and_then(|n| number(attr(n, "w")))
        .unwrap_or(0.0)
        / 15.0;
    Ok(Table {
        columns,
        rows,
        alignment,
        indent,
    })
}

pub(crate) const DRAWING_SOURCE: usize = 500_000;

#[derive(Clone, Debug, PartialEq)]
pub struct InlineRaster {
    pub bytes: Arc<Vec<u8>>,
    pub width: f32,
    pub height: f32,
}

pub(crate) fn drawings<'a, 'i>(paragraph: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    paragraph.descendants().filter(|n| is(*n, "drawing"))
}

pub(crate) fn standalone(paragraph: Node<'_, '_>) -> bool {
    paragraph.parent().is_some_and(|n| is(n, "body"))
        && drawings(paragraph).count() == 1
        && !paragraph.descendants().any(|n| {
            ["t", "tab", "br", "cr", "noBreakHyphen"]
                .iter()
                .any(|name| is(n, name))
        })
}

pub(super) fn read_raster(
    drawing: Node<'_, '_>,
    package: &Package,
    main: &str,
    rel_path: &str,
) -> Result<InlineRaster, String> {
    const WP: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
    const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
    const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let local = |n: Node<'_, '_>, ns: &str, tag: &str| n.has_tag_name((ns, tag));
    let inline = drawing
        .children()
        .find(|n| local(*n, WP, "inline"))
        .ok_or("Плавающие рисунки пока не отображаются")?;
    if drawing.descendants().any(|n| {
        local(n, A, "srcRect")
            || (local(n, A, "xfrm")
                && n.attributes()
                    .any(|a| matches!(a.name(), "rot" | "flipH" | "flipV")))
    }) {
        return Err("Обрезка и поворот рисунка пока не поддерживаются".into());
    }
    let ext = inline
        .children()
        .find(|n| local(*n, WP, "extent"))
        .ok_or("У рисунка нет размеров")?;
    let dimension = |key| {
        ext.attribute(key)
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0 && *v <= 95_250_000.0)
            .map(|v| (v / 9525.0) as f32)
            .ok_or("Некорректный размер рисунка")
    };
    let width = dimension("cx")?;
    let height = dimension("cy")?;
    let blip = inline
        .descendants()
        .find(|n| local(*n, A, "blip"))
        .ok_or("Поддерживаются только встроенные растровые рисунки")?;
    let id = blip
        .attribute((R, "embed"))
        .ok_or("Внешние рисунки не загружаются")?;
    let rels = package.text(rel_path)?;
    let rels = parse_xml(&rels).map_err(|_| "Некорректные связи рисунка")?;
    let rel = rels
        .descendants()
        .find(|n| {
            n.is_element() && n.tag_name().name() == "Relationship" && n.attribute("Id") == Some(id)
        })
        .ok_or("Не найдена связь рисунка")?;
    if rel.attribute("TargetMode") == Some("External")
        || !rel.attribute("Type").is_some_and(|v| v.ends_with("/image"))
    {
        return Err("Внешняя или неподходящая связь рисунка".into());
    }
    let path = resolve_target(main, rel.attribute("Target").ok_or("У рисунка нет адреса")?)?;
    let bytes = package.picture_bytes(&path)?;
    if !(bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.starts_with(b"\xff\xd8\xff")) {
        return Err("Поддерживаются только встроенные PNG и JPEG".into());
    }
    Ok(InlineRaster {
        bytes,
        width,
        height,
    })
}

pub(super) fn read(
    xml: &Xml<'_>,
    styles: &Xml<'_>,
    package: &Package,
    main: &str,
    rel_path: &str,
) -> Result<Option<ViewContent>, String> {
    if !xml
        .descendants()
        .any(|n| is(n, "tbl") || (is(n, "p") && standalone(n)))
    {
        return Ok(None);
    }
    let indices: HashMap<_, _> = xml
        .descendants()
        .filter(|n| is(*n, "p") && !n.ancestors().any(|a| is(a, "txbxContent")))
        .enumerate()
        .map(|(i, p)| (p.id(), i))
        .collect();
    let body = xml
        .descendants()
        .find(|n| is(*n, "body"))
        .ok_or("Нет тела документа")?;
    let mut blocks = Vec::new();
    for node in body.children().filter(|n| n.is_element()) {
        if is(node, "p") {
            let index = *indices.get(&node.id()).ok_or("Не найден абзац")?;
            if standalone(node) {
                let raster = read_raster(drawings(node).next().unwrap(), package, main, rel_path)?;
                blocks.push(Block::Picture(Picture {
                    bytes: raster.bytes,
                    width: raster.width,
                    height: raster.height,
                    paragraph: index,
                }));
            } else {
                blocks.push(Block::Paragraph(index));
            }
        } else if is(node, "tbl") {
            blocks.push(Block::Table(table(node, styles, &indices)?));
        } else if !is(node, "sectPr") {
            return Err("Неподдерживаемый блок в сложном документе".into());
        }
    }
    Ok(Some(ViewContent { blocks }))
}

#[cfg(test)]
mod tests {
    use super::*;
    const FIXTURE: &[u8] = include_bytes!("../../../tests/fixtures/blocks.docx");
    fn changed(part: &str, from: &str, to: &str) -> Result<Document, String> {
        let package = Package::parse(FIXTURE.to_vec()).unwrap();
        let bytes = package
            .replace(&BTreeMap::from([(
                part.into(),
                package.text(part).unwrap().replace(from, to).into_bytes(),
            )]))
            .unwrap();
        Document::from_bytes(bytes)
    }
    #[test]
    fn reads_every_cell_and_preserves_original_copy() {
        let doc = Document::from_bytes(FIXTURE.to_vec()).unwrap();
        assert_eq!(doc.paragraphs.len(), 106);
        let blocks = &doc.content.as_ref().unwrap().blocks;
        assert_eq!(blocks.len(), 4);
        let Block::Picture(picture) = &blocks[1] else {
            panic!("image missing")
        };
        assert_eq!((picture.width, picture.height), (96.0, 48.0));
        let Block::Table(table) = &blocks[2] else {
            panic!("table missing")
        };
        assert_eq!(table.columns, vec![200.0, 200.0]);
        assert_eq!(table.rows.len(), 52);
        assert!(table.rows[0].header);
        assert_eq!(table.rows[0].cells[0].fill, Some([223, 237, 227]));
        assert_eq!(
            table.rows[0].cells[0].margins,
            [3.0, 55.0 / 15.0, 3.0, 55.0 / 15.0]
        );
        assert_eq!(table.rows.last().unwrap().cells[0].span, 2);
        assert!(doc.paragraphs[0].edit_block.is_none());
        assert!(doc.paragraphs[1].edit_block.is_some());
        assert_eq!(doc.to_docx(Some(9)).unwrap(), FIXTURE);
        let mut altered = doc.clone();
        altered.paragraphs[0].runs[0].text = "Changed".into();
        let changed = Document::from_bytes(altered.to_docx(None).unwrap()).unwrap();
        assert_eq!(changed.paragraphs[0].text(), "Changed");
        let editor = editor::Editor::new(doc);
        assert!(
            editor
                .prepare(editor::Edit::Insert("Preview".into()))
                .unwrap()
                .is_some()
        );
        assert!(!editor.dirty());
    }
    #[test]
    fn malformed_grid_and_invalid_vertical_merges_fail_explicitly() {
        let error = changed("word/document.xml", "w:val=\"2\"", "w:val=\"0\"").unwrap_err();
        assert!(error.contains("сетку"));
        assert!(
            changed("word/document.xml", "<w:tcPr>", "<w:tcPr><w:vMerge/>")
                .unwrap_err()
                .contains("объединен")
        );
        assert!(
            changed("word/document.xml", "<w:tbl>", "<w:tbl><w:tbl/>")
                .unwrap_err()
                .contains("Вложенные")
        );
    }
    #[test]
    fn external_and_floating_images_are_rejected_before_rendering() {
        assert!(
            changed(
                "word/_rels/document.xml.rels",
                "Target=\"media/sample.png\"",
                "TargetMode=\"External\" Target=\"https://example.invalid/image.png\""
            )
            .unwrap_err()
            .contains("Внешняя")
        );
        assert!(
            changed("word/document.xml", "wp:inline", "wp:anchor")
                .unwrap_err()
                .contains("Плавающие")
        );
        let mixed = changed(
            "word/document.xml",
            "<w:drawing>",
            "<w:t>mixed</w:t><w:drawing>",
        )
        .unwrap();
        assert_eq!(mixed.paragraphs[1].text(), "mixed\u{fffc}");
        assert_eq!(mixed.paragraphs[1].inline_pictures().count(), 1);
    }
}
