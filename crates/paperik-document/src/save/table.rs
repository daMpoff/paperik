use super::*;
use crate::{Block, Table, VerticalMerge};
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableAction {
    InsertRow { after: bool },
    DeleteRow,
    InsertColumn { after: bool },
    DeleteColumn,
    Width { twips: u32 },
    Height { twips: u32 },
    AutoHeight,
    Merge { end_paragraph: usize },
    Split,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableEdit {
    pub paragraph: usize,
    pub action: TableAction,
}

impl Document {
    /// (Block index, physical row, cell index in that row).
    pub fn table_cell(&self, paragraph: usize) -> Option<(usize, usize, usize)> {
        for (block, value) in self.content.as_ref()?.blocks.iter().enumerate() {
            let Block::Table(table) = value else { continue };
            for (row, value) in table.rows.iter().enumerate() {
                if let Some(cell) = value.cells.iter().position(|c| {
                    c.merge != Some(VerticalMerge::Continue) && c.paragraphs.contains(&paragraph)
                }) {
                    return Some((block, row, cell));
                }
            }
        }
        None
    }

    pub(crate) fn edit_table(&self, edit: TableEdit) -> Result<(Document, usize), String> {
        if self
            .package
            .names()
            .any(|name| name.to_ascii_lowercase().starts_with("_xmlsignatures/"))
        {
            return Err("Изменение DOCX с цифровой подписью пока не поддерживается".into());
        }
        let (block, row, cell) = self
            .table_cell(edit.paragraph)
            .ok_or("Поставьте курсор в ячейку таблицы")?;
        let table_number = self.content.as_ref().unwrap().blocks[..block]
            .iter()
            .filter(|block| matches!(block, Block::Table(_)))
            .count();
        let prepared = self.prepared_xml()?;
        let (main, mut source) = match prepared {
            Some((name, xml)) => {
                let materialized =
                    Document::from_package(self.package.with_main(name.clone(), xml.clone())?)?;
                if !same_document(self, &materialized) {
                    return Err("Текущие правки текста нельзя перенести в структуру таблицы".into());
                }
                (name, xml)
            }
            None => {
                let name = relationship(&self.package, "_rels/.rels", "", "officeDocument")?
                    .ok_or("Не найдена основная часть DOCX")?;
                let xml = self.package.text(&name)?;
                (name, xml)
            }
        };
        let xml = parse_xml(&source).map_err(|e| e.to_string())?;
        let node = xml
            .descendants()
            .filter(|n| is(*n, "tbl"))
            .nth(table_number)
            .ok_or("Таблица не найдена в исходном XML")?;
        let Block::Table(table) = &self.content.as_ref().unwrap().blocks[block] else {
            unreachable!()
        };
        let mut plan = Plan::new(node, table)?;
        let target = plan.apply(self, block, row, cell, edit.action)?;
        let replacement = plan.render(&source)?;
        let range = node.range();
        if source.len() - range.len() + replacement.len() > MAX_XML {
            return Err("XML документа превышает 16 МиБ".into());
        }
        source.replace_range(range, &replacement);
        let result = Document::from_package(self.package.with_main(main, source)?)?;
        let Block::Table(table) = &result
            .content
            .as_ref()
            .ok_or("Таблица потеряна после правки")?
            .blocks[block]
        else {
            return Err("Таблица потеряна после правки".into());
        };
        let mut row = target.0.min(table.rows.len() - 1);
        let paragraph = loop {
            let cell = table.rows[row]
                .cells
                .iter()
                .find(|c| (c.column..c.column + c.span).contains(&target.1))
                .or_else(|| table.rows[row].cells.last())
                .ok_or("После правки нет ячейки")?;
            if cell.merge != Some(VerticalMerge::Continue) {
                break cell.paragraphs[0];
            }
            row = row
                .checked_sub(1)
                .ok_or("Не найдена верхняя ячейка объединения")?;
        };
        Ok((result, paragraph))
    }
}

const MIN_WIDTH: u32 = 300; // 20 DIP, with room for usual cell margins.
const MIN_HEIGHT: u32 = 120; // 8 DIP, actual height can grow to fit text.

#[derive(Clone)]
enum CellText<'a, 'i> {
    Existing,
    Empty,
    Combined(Vec<Node<'a, 'i>>),
}

#[derive(Clone)]
struct Cell<'a, 'i> {
    node: Node<'a, 'i>,
    span: usize,
    original_span: usize,
    original_width: u32,
    merge: Option<VerticalMerge>,
    original_merge: Option<VerticalMerge>,
    text: CellText<'a, 'i>,
}

impl Cell<'_, '_> {
    fn blank(&self, span: usize, merge: Option<VerticalMerge>) -> Self {
        Self {
            span,
            merge,
            text: CellText::Empty,
            ..self.clone()
        }
    }
    fn ensure_removable(&self) -> Result<(), String> {
        if self
            .node
            .children()
            .filter(|n| is(*n, "p"))
            .any(|p| edit_block(p).is_some())
        {
            return Err("Ячейка содержит защищённый абзац или служебный объект".into());
        }
        Ok(())
    }
}

#[derive(Clone)]
struct Row<'a, 'i> {
    node: Node<'a, 'i>,
    cells: Vec<Cell<'a, 'i>>,
    header: bool,
    inserted: bool,
    height: Option<u32>,
}

impl Row<'_, '_> {
    fn span_start(&self, cell: usize) -> usize {
        self.cells[..cell].iter().map(|c| c.span).sum()
    }
    fn covering(&self, column: usize) -> Option<(usize, usize)> {
        let mut start = 0;
        for (index, cell) in self.cells.iter().enumerate() {
            if column < start + cell.span {
                return Some((index, start));
            }
            start += cell.span;
        }
        None
    }
    fn exact_range(&self, columns: Range<usize>) -> Result<Range<usize>, String> {
        let (first, start) = self.covering(columns.start).ok_or("Столбец вне таблицы")?;
        let (last, last_start) = self
            .covering(columns.end - 1)
            .ok_or("Столбец вне таблицы")?;
        if start != columns.start || last_start + self.cells[last].span != columns.end {
            return Err("Граница выделения проходит через объединённую ячейку".into());
        }
        Ok(first..last + 1)
    }
}

struct Plan<'a, 'i> {
    node: Node<'a, 'i>,
    grid: Node<'a, 'i>,
    original_widths: Vec<u32>,
    widths: Vec<u32>,
    rows: Vec<Row<'a, 'i>>,
}

impl<'a, 'i> Plan<'a, 'i> {
    fn new(node: Node<'a, 'i>, table: &Table) -> Result<Self, String> {
        if node.descendants().any(|n| {
            n.is_comment()
                || n.is_pi()
                || (n.is_element()
                    && matches!(n.tag_name().namespace(), Some(W | WS))
                    && (n.tag_name().name().ends_with("Change")
                        || matches!(n.tag_name().name(), "cellIns" | "cellDel" | "cellMerge")))
        }) {
            return Err(
                "Структурные правки таблиц с XML-комментариями или исправлениями пока недоступны"
                    .into(),
            );
        }
        let grid = child(node, "tblGrid").ok_or("Нет сетки таблицы")?;
        let widths: Vec<_> = grid
            .children()
            .filter(|n| n.is_element())
            .map(|n| {
                if !is(n, "gridCol") {
                    return Err("Сетка содержит неизвестные свойства");
                }
                attr(n, "w")
                    .and_then(|v| v.parse::<u32>().ok())
                    .filter(|v| *v > 0 && *v <= 150_000)
                    .ok_or("Размеры сетки должны быть целыми двадцатыми долями пункта")
            })
            .collect::<Result<_, _>>()?;
        if widths.len() != table.columns.len() {
            return Err("Сетка не совпадает с моделью".into());
        }
        let xml_rows: Vec<_> = node.children().filter(|n| is(*n, "tr")).collect();
        if xml_rows.len() != table.rows.len() {
            return Err("Строки не совпадают с моделью".into());
        }
        let mut rows = Vec::new();
        for (node, row) in xml_rows.into_iter().zip(&table.rows) {
            let cells: Vec<_> = node.children().filter(|n| is(*n, "tc")).collect();
            if cells.len() != row.cells.len() {
                return Err("Ячейки не совпадают с моделью".into());
            }
            rows.push(Row {
                node,
                header: row.header,
                inserted: false,
                height: None,
                cells: cells
                    .into_iter()
                    .zip(&row.cells)
                    .map(|(node, cell)| Cell {
                        node,
                        span: cell.span,
                        original_span: cell.span,
                        original_width: widths[cell.column..cell.column + cell.span].iter().sum(),
                        merge: cell.merge,
                        original_merge: cell.merge,
                        text: CellText::Existing,
                    })
                    .collect(),
            });
        }
        Ok(Self {
            node,
            grid,
            original_widths: widths.clone(),
            widths,
            rows,
        })
    }

    fn apply(
        &mut self,
        doc: &Document,
        block: usize,
        row: usize,
        cell: usize,
        action: TableAction,
    ) -> Result<(usize, usize), String> {
        let column = self.rows[row].span_start(cell);
        let span = self.rows[row].cells[cell].span;
        match action {
            TableAction::InsertRow { after } => {
                if self.rows.len() >= 10_000 {
                    return Err("Достигнут предел строк таблицы".into());
                }
                let index = row + usize::from(after);
                let mut added = self.rows[row].clone();
                added.inserted = true;
                added.header = self.rows.get(index).is_some_and(|r| r.header);
                let next = self.rows.get(index);
                let mut start = 0;
                for cell in &mut added.cells {
                    let continues = next
                        .and_then(|r| r.covering(start).map(|(i, c)| (&r.cells[i], c)))
                        .is_some_and(|(next, c)| {
                            c == start
                                && next.span == cell.span
                                && next.merge == Some(VerticalMerge::Continue)
                        });
                    *cell = cell.blank(cell.span, continues.then_some(VerticalMerge::Continue));
                    start += cell.span;
                }
                self.rows.insert(index, added);
                return Ok((index, column));
            }
            TableAction::DeleteRow => {
                if self.rows.len() == 1 {
                    return Err("В таблице должна остаться хотя бы одна строка".into());
                }
                if self.rows[row].cells.iter().any(|c| c.merge.is_some()) {
                    return Err("Сначала разъедините вертикальные ячейки в удаляемой строке".into());
                }
                for cell in &self.rows[row].cells {
                    cell.ensure_removable()?;
                }
                self.rows.remove(row);
            }
            TableAction::InsertColumn { after } => {
                if self.widths.len() >= 128 {
                    return Err("Достигнут предел столбцов таблицы".into());
                }
                let index = column + if after { span } else { 0 };
                let donor = if after { index - 1 } else { index };
                let spare =
                    ((doc.page.width - doc.page.left - doc.page.right) * 15.0).floor() as u32;
                let spare = spare.saturating_sub(self.widths.iter().sum());
                let width = if spare >= MIN_WIDTH {
                    self.widths[donor].max(MIN_WIDTH).min(spare)
                } else {
                    let width = self.widths[donor] / 2;
                    if width < MIN_WIDTH {
                        return Err(
                            "Недостаточно места для столбца. Увеличьте ширину соседнего столбца"
                                .into(),
                        );
                    }
                    self.widths[donor] -= width;
                    width
                };
                for row in &mut self.rows {
                    let (i, start) = row.covering(index).unwrap_or((row.cells.len(), index));
                    if start < index {
                        row.cells[i].span += 1;
                    } else {
                        let template = if after {
                            i.saturating_sub(1)
                        } else {
                            i.min(row.cells.len() - 1)
                        };
                        row.cells.insert(i, row.cells[template].blank(1, None));
                    }
                }
                self.widths.insert(index, width);
                return Ok((row, index));
            }
            TableAction::DeleteColumn => {
                if self.widths.len() == 1 {
                    return Err("В таблице должен остаться хотя бы один столбец".into());
                }
                if span > 1 {
                    return Err(
                        "Сначала разъедините выбранную ячейку, чтобы указать конкретный столбец"
                            .into(),
                    );
                }
                for row in &mut self.rows {
                    let (i, _) = row.covering(column).ok_or("Столбец вне таблицы")?;
                    if row.cells[i].span == 1 {
                        row.cells[i].ensure_removable()?;
                        row.cells.remove(i);
                    } else {
                        row.cells[i].span -= 1;
                    }
                }
                self.widths.remove(column);
            }
            TableAction::Width { twips } => {
                let current: u32 = self.widths[column..column + span].iter().sum();
                let delta_twips = i64::from(twips) - i64::from(current);
                let index = column + span - 1;
                let neighbour = if index + 1 < self.widths.len() {
                    Some(index + 1)
                } else {
                    column.checked_sub(1)
                };
                let width = i64::from(self.widths[index]) + delta_twips;
                if !(i64::from(MIN_WIDTH)..=150_000).contains(&width) {
                    return Err("Столбец выходит за допустимые размеры".into());
                }
                if let Some(next) = neighbour {
                    let width = i64::from(self.widths[next]) - delta_twips;
                    if !(i64::from(MIN_WIDTH)..=150_000).contains(&width) {
                        return Err("Соседнему столбцу не хватает ширины".into());
                    }
                    self.widths[next] = width as u32;
                }
                self.widths[index] = width as u32;
            }
            TableAction::Height { twips } => {
                if !(MIN_HEIGHT..=150_000).contains(&twips) {
                    return Err("Строка выходит за допустимые размеры".into());
                }
                self.rows[row].height = Some(twips);
            }
            TableAction::AutoHeight => self.rows[row].height = Some(0),
            TableAction::Merge { end_paragraph } => {
                let (other, end_row, end_cell) = doc
                    .table_cell(end_paragraph)
                    .ok_or("Выделите ячейки одной таблицы")?;
                if other != block {
                    return Err("Выделите ячейки одной таблицы".into());
                }
                let end_col = self.rows[end_row].span_start(end_cell);
                let left = column.min(end_col);
                let right = (column + span).max(end_col + self.rows[end_row].cells[end_cell].span);
                let top = row.min(end_row);
                let bottom = row.max(end_row) + 1;
                self.merge(top..bottom, left..right)?;
                return Ok((top, left));
            }
            TableAction::Split => self.split(row, cell, column)?,
        }
        Ok((
            row.min(self.rows.len() - 1),
            column.min(self.widths.len() - 1),
        ))
    }

    fn merge(&mut self, rows: Range<usize>, columns: Range<usize>) -> Result<(), String> {
        let ranges = self.rows[rows.clone()]
            .iter()
            .map(|row| row.exact_range(columns.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        if rows.len() == 1 && ranges[0].len() == 1 {
            return Err("Выделите хотя бы две ячейки".into());
        }
        let header = self.rows[rows.start].header;
        let mut paragraphs = Vec::new();
        for (row, cells) in self.rows[rows.clone()].iter().zip(&ranges) {
            if row.header != header {
                return Err("Объединение не может пересекать границу повторяемой шапки".into());
            }
            for cell in &row.cells[cells.clone()] {
                if cell.merge.is_some() {
                    return Err("Сначала разъедините вертикальные ячейки в выделении".into());
                }
                cell.ensure_removable()?;
                paragraphs.extend(cell.node.children().filter(|n| is(*n, "p")));
            }
        }
        let vertical = rows.len() > 1;
        for (offset, (row, cells)) in self.rows[rows].iter_mut().zip(ranges).enumerate() {
            let mut merged = row.cells[cells.start].blank(columns.len(), None);
            if offset == 0 {
                merged.text = CellText::Combined(std::mem::take(&mut paragraphs));
                merged.merge = vertical.then_some(VerticalMerge::Start);
            } else {
                merged.merge = Some(VerticalMerge::Continue);
            }
            row.cells.splice(cells, [merged]);
        }
        Ok(())
    }

    fn split(&mut self, row: usize, cell: usize, column: usize) -> Result<(), String> {
        let anchor = &self.rows[row].cells[cell];
        let span = anchor.span;
        if span == 1 && anchor.merge != Some(VerticalMerge::Start) {
            return Err("Выбранная ячейка уже раздельная".into());
        }
        for index in row..self.rows.len() {
            let (i, start) = self.rows[index]
                .covering(column)
                .ok_or("Не найдена объединённая ячейка")?;
            let original = &self.rows[index].cells[i];
            if index > row
                && (start != column
                    || original.span != span
                    || original.merge != Some(VerticalMerge::Continue))
            {
                break;
            }
            let mut cells = vec![original.blank(1, None); span];
            if index == row {
                cells[0].text = CellText::Existing;
            }
            self.rows[index].cells.splice(i..i + 1, cells);
        }
        Ok(())
    }
}

fn tag(uri: &str, name: &str, attributes: &str) -> String {
    format!("<pk:{name} xmlns:pk=\"{uri}\" {attributes}/>")
}

fn wrap(source: &str, node: Node<'_, '_>, inner: &str) -> String {
    let raw = raw(source, node);
    if let Some(end) = raw.rfind("</") {
        let mut quote = None;
        let open = raw
            .char_indices()
            .find_map(|(i, c)| {
                if let Some(q) = quote {
                    if c == q {
                        quote = None;
                    }
                } else if c == '"' || c == '\'' {
                    quote = Some(c);
                } else if c == '>' {
                    return Some(i + 1);
                }
                None
            })
            .unwrap();
        format!("{}{inner}{}", &raw[..open], &raw[end..])
    } else {
        let (open, prefix) = wrapper(node, node.tag_name().name(), false);
        format!("{open}{inner}</{prefix}:{}>", node.tag_name().name())
    }
}

fn properties(
    source: &str,
    node: Option<Node<'_, '_>>,
    uri: &str,
    name: &str,
    order: &[&str],
    updates: &[(&str, Option<String>)],
) -> String {
    let rank = |name| order.iter().position(|n| *n == name).unwrap_or(order.len());
    let mut children: Vec<_> = node
        .into_iter()
        .flat_map(|node| node.children())
        .filter(|n| n.is_element())
        .map(|n| (n.tag_name().name(), raw(source, n).to_owned()))
        .collect();
    for (name, value) in updates {
        if let Some(i) = children.iter().position(|(key, _)| key == name) {
            if let Some(value) = value {
                children[i].1 = value.clone();
            } else {
                children.remove(i);
            }
        } else if let Some(value) = value {
            let i = children
                .iter()
                .position(|(key, _)| rank(*key) > rank(*name))
                .unwrap_or(children.len());
            children.insert(i, (name, value.clone()));
        }
    }
    let inner: String = children.into_iter().map(|(_, value)| value).collect();
    if let Some(node) = node {
        wrap(source, node, &inner)
    } else if inner.is_empty() {
        String::new()
    } else {
        format!("<pk:{name} xmlns:pk=\"{uri}\">{inner}</pk:{name}>")
    }
}

const TABLE_PROPERTIES: &[&str] = &[
    "tblStyle",
    "tblpPr",
    "tblOverlap",
    "bidiVisual",
    "tblStyleRowBandSize",
    "tblStyleColBandSize",
    "tblW",
    "jc",
    "tblCellSpacing",
    "tblInd",
    "tblBorders",
    "shd",
    "tblLayout",
    "tblCellMar",
    "tblLook",
    "tblCaption",
    "tblDescription",
    "tblPrChange",
];
const ROW_PROPERTIES: &[&str] = &[
    "cnfStyle",
    "divId",
    "gridBefore",
    "gridAfter",
    "wBefore",
    "wAfter",
    "cantSplit",
    "trHeight",
    "tblHeader",
    "tblCellSpacing",
    "jc",
    "hidden",
    "trPrChange",
];
const CELL_PROPERTIES: &[&str] = &[
    "cnfStyle",
    "tcW",
    "gridSpan",
    "hMerge",
    "vMerge",
    "tcBorders",
    "shd",
    "noWrap",
    "tcMar",
    "textDirection",
    "tcFitText",
    "vAlign",
    "hideMark",
    "headers",
    "tcPrChange",
];

fn blank_paragraph(source: &str, cell: Node<'_, '_>, uri: &str) -> String {
    let Some(p) = child(cell, "p") else {
        return tag(uri, "p", "");
    };
    if edit_block(p).is_some() {
        return tag(uri, "p", "");
    }
    let props = child(p, "pPr");
    let inherited = props
        .and_then(|p| child(p, "rPr"))
        .or_else(|| child(p, "r").and_then(|r| child(r, "rPr")));
    let mut inner = String::new();
    if let Some(props) = props {
        for n in props
            .children()
            .filter(|n| n.is_element() && !is(*n, "rPr") && !is(*n, "pageBreakBefore"))
        {
            inner.push_str(raw(source, n));
        }
    }
    if let Some(inherited) = inherited {
        let (opening, prefix) = wrapper(inherited, "rPr", false);
        inner.push_str(&opening);
        for child in inherited.children() {
            inner.push_str(raw(source, child));
        }
        inner.push_str(&format!("</{prefix}:rPr>"));
    }
    let (opening, prefix) = wrapper(p, "p", true);
    if inner.is_empty() {
        format!("{opening}</{prefix}:p>")
    } else {
        let ppr = if let Some(props) = props {
            wrap(source, props, &inner)
        } else {
            format!("<{prefix}:pPr>{inner}</{prefix}:pPr>")
        };
        format!("{opening}{ppr}</{prefix}:p>")
    }
}

impl Cell<'_, '_> {
    fn render(&self, source: &str, uri: &str, width: u32) -> String {
        let cp = child(self.node, "tcPr");
        let mut updates = Vec::new();
        if width != self.original_width
            || self.span != self.original_span
            || !matches!(self.text, CellText::Existing)
        {
            updates.push((
                "tcW",
                Some(tag(
                    uri,
                    "tcW",
                    &format!("pk:w=\"{width}\" pk:type=\"dxa\""),
                )),
            ));
        }
        if self.span != self.original_span {
            updates.push((
                "gridSpan",
                (self.span > 1).then(|| tag(uri, "gridSpan", &format!("pk:val=\"{}\"", self.span))),
            ));
        }
        if self.merge != self.original_merge {
            updates.push((
                "vMerge",
                self.merge.map(|merge| {
                    tag(
                        uri,
                        "vMerge",
                        match merge {
                            VerticalMerge::Start => "pk:val=\"restart\"",
                            VerticalMerge::Continue => "pk:val=\"continue\"",
                        },
                    )
                }),
            ));
        }
        if updates.is_empty() && matches!(self.text, CellText::Existing) {
            return raw(source, self.node).into();
        }
        let mut inner = properties(source, cp, uri, "tcPr", CELL_PROPERTIES, &updates);
        match &self.text {
            CellText::Existing => {
                for p in self.node.children().filter(|n| is(*n, "p")) {
                    inner.push_str(raw(source, p));
                }
            }
            CellText::Empty => inner.push_str(&blank_paragraph(source, self.node, uri)),
            CellText::Combined(paragraphs) => {
                for p in paragraphs {
                    let (opening, prefix) = wrapper(*p, "p", false);
                    inner.push_str(&opening);
                    for n in p.children() {
                        inner.push_str(raw(source, n));
                    }
                    inner.push_str(&format!("</{prefix}:p>"));
                }
            }
        }
        wrap(source, self.node, &inner)
    }
}

impl Plan<'_, '_> {
    fn render(&self, source: &str) -> Result<String, String> {
        let uri = self.node.tag_name().namespace().unwrap_or(W);
        let changed = self.widths != self.original_widths;
        let props = child(self.node, "tblPr");
        let mut inner = if changed {
            properties(
                source,
                props,
                uri,
                "tblPr",
                TABLE_PROPERTIES,
                &[
                    (
                        "tblW",
                        Some(tag(
                            uri,
                            "tblW",
                            &format!(
                                "pk:w=\"{}\" pk:type=\"dxa\"",
                                self.widths.iter().sum::<u32>()
                            ),
                        )),
                    ),
                    (
                        "tblLayout",
                        Some(tag(uri, "tblLayout", "pk:type=\"fixed\"")),
                    ),
                ],
            )
        } else {
            props.map_or(String::new(), |n| raw(source, n).into())
        };
        if changed {
            let grid: String = self
                .widths
                .iter()
                .map(|w| tag(uri, "gridCol", &format!("pk:w=\"{w}\"")))
                .collect();
            inner.push_str(&wrap(source, self.grid, &grid));
        } else {
            inner.push_str(raw(source, self.grid));
        }
        for row in &self.rows {
            let rp = child(row.node, "trPr");
            let mut updates = Vec::new();
            if row.inserted {
                let source_header = rp
                    .and_then(|n| child(n, "tblHeader"))
                    .is_some_and(crate::enabled);
                if row.header != source_header {
                    updates.push(("tblHeader", row.header.then(|| tag(uri, "tblHeader", ""))));
                }
            }
            if let Some(height) = row.height {
                updates.push((
                    "trHeight",
                    (height != 0).then(|| {
                        tag(
                            uri,
                            "trHeight",
                            &format!("pk:val=\"{height}\" pk:hRule=\"atLeast\""),
                        )
                    }),
                ));
            }
            let mut row_xml = if updates.is_empty() {
                rp.map_or(String::new(), |n| raw(source, n).into())
            } else {
                properties(source, rp, uri, "trPr", ROW_PROPERTIES, &updates)
            };
            let mut unchanged = !row.inserted
                && updates.is_empty()
                && row
                    .node
                    .children()
                    .filter(|n| is(*n, "tc"))
                    .eq(row.cells.iter().map(|cell| cell.node));
            let mut column = 0;
            for cell in &row.cells {
                let end = column + cell.span;
                let width = self
                    .widths
                    .get(column..end)
                    .ok_or("Строка выходит за новую сетку")?
                    .iter()
                    .sum();
                let cell_xml = cell.render(source, uri, width);
                unchanged &= cell_xml == raw(source, cell.node);
                row_xml.push_str(&cell_xml);
                column = end;
                if row_xml.len() + inner.len() > MAX_XML {
                    return Err("XML таблицы превышает 16 МиБ".into());
                }
            }
            if column != self.widths.len() {
                return Err("Новая строка не заполняет сетку".into());
            }
            if unchanged {
                inner.push_str(raw(source, row.node));
            } else {
                inner.push_str(&wrap(source, row.node, &row_xml));
            }
        }
        Ok(wrap(source, self.node, &inner))
    }
}

#[cfg(test)]
mod tests;
