//! Surgical OOXML export. Unchanged XML/parts are retained, never reserialized wholesale.
mod blocks;
mod picture;
mod table;
use crate::{Document, Node, Paragraph, Run, W, WS, attr, child, is, parse_xml, relationship};
pub use picture::{PictureAction, PictureEdit};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
pub use table::{TableAction, TableEdit};
const MAX_XML: usize = 16 * 1024 * 1024;

pub(crate) fn edit_block(paragraph: Node<'_, '_>) -> Option<&'static str> {
    if crate::blocks::standalone(paragraph) {
        return Some("отдельный рисунок");
    }
    if paragraph.children().any(|n| {
        n.is_comment()
            || (n.is_text() && !n.text().unwrap_or("").trim().is_empty())
            || (n.is_element() && !["pPr", "r", "proofErr"].iter().any(|tag| is(n, tag)))
    }) {
        return Some("закладки, ссылки, поля или неподдерживаемая структура");
    }
    if paragraph.descendants().any(|n| {
        is(n, "sectPr")
            || (n.is_element()
                && matches!(n.tag_name().namespace(), Some(W | WS))
                && n.tag_name().name().ends_with("Change"))
    }) {
        return Some("граница раздела или отслеживаемые изменения");
    }
    for run in paragraph.children().filter(|n| is(*n, "r")) {
        if run.children().any(|n| {
            n.is_comment()
                || (n.is_text() && !n.text().unwrap_or("").trim().is_empty())
                || (n.is_element()
                    && ![
                        "rPr",
                        "t",
                        "tab",
                        "br",
                        "cr",
                        "noBreakHyphen",
                        "lastRenderedPageBreak",
                        "drawing",
                    ]
                    .iter()
                    .any(|tag| is(n, tag)))
        }) || child(run, "rPr")
            .and_then(|n| child(n, "vanish"))
            .is_some_and(crate::enabled)
            || run.children().any(|n| {
                is(n, "br")
                    && (attr(n, "type").is_some_and(|t| t != "textWrapping")
                        || attr(n, "clear").is_some())
            })
        {
            return Some("скрытый текст, специальные разрывы или служебные фрагменты");
        }
        if run
            .children()
            .filter(|n| n.is_element() && !is(*n, "rPr") && !is(*n, "drawing"))
            .any(|n| {
                n.children().any(|c| c.is_element() || c.is_comment())
                    || n.attributes().any(|a| {
                        !(is(n, "t")
                            && a.namespace() == Some("http://www.w3.org/XML/1998/namespace")
                            && a.name() == "space")
                            && !(is(n, "br")
                                && matches!(a.namespace(), Some(W | WS))
                                && a.name() == "type"
                                && a.value() == "textWrapping")
                    })
            })
        {
            return Some("неподдерживаемые свойства текстовых элементов");
        }
        if !run.children().any(|n| {
            (is(n, "t") && !n.text().unwrap_or("").is_empty())
                || ["tab", "br", "cr", "noBreakHyphen", "drawing"]
                    .iter()
                    .any(|tag| is(n, tag))
        }) {
            return Some("пустой фрагмент со своим оформлением");
        }
    }
    None
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\r', "&#13;")
}
fn attribute_escape(text: &str) -> String {
    escape(text).replace('\n', "&#10;").replace('\t', "&#9;")
}
fn raw<'a>(source: &'a str, node: Node<'_, '_>) -> &'a str {
    &source[node.range()]
}
fn wrapper(node: Node<'_, '_>, local: &str, strip_ids: bool) -> (String, String) {
    let namespaces: BTreeMap<_, _> = node.namespaces().map(|n| (n.name(), n.uri())).collect();
    let mut prefix = "pk".to_owned();
    while namespaces.contains_key(&Some(prefix.as_str())) {
        prefix.push('x');
    }
    let uri = node.tag_name().namespace().unwrap_or(W);
    let mut result = format!(
        "<{prefix}:{local} xmlns:{prefix}=\"{}\"",
        attribute_escape(uri)
    );
    for (name, uri) in &namespaces {
        if *name == Some("xml") {
            continue;
        }
        result.push_str(&match name {
            Some(name) => format!(" xmlns:{name}=\"{}\"", attribute_escape(uri)),
            None => format!(" xmlns=\"{}\"", attribute_escape(uri)),
        });
    }
    for attribute in node.attributes() {
        if strip_ids
            && attribute.namespace() == Some("http://schemas.microsoft.com/office/word/2010/wordml")
            && matches!(attribute.name(), "paraId" | "textId")
        {
            continue;
        }
        let name = if let Some(uri) = attribute.namespace() {
            let prefix = if uri == "http://www.w3.org/XML/1998/namespace" {
                "xml"
            } else {
                namespaces
                    .iter()
                    .find_map(|(name, value)| if *value == uri { *name } else { None })
                    .unwrap_or("xml")
            };
            format!("{prefix}:{}", attribute.name())
        } else {
            attribute.name().into()
        };
        result.push_str(&format!(
            " {name}=\"{}\"",
            attribute_escape(attribute.value())
        ));
    }
    result.push('>');
    (result, prefix)
}
fn same_paragraph(a: &Paragraph, b: &Paragraph) -> bool {
    a.style == b.style
        && a.mark_font == b.mark_font
        && a.style_id == b.style_id
        && a.list == b.list
        && a.list_label == b.list_label
        && a.page_field == b.page_field
        && same_runs(&a.runs, &b.runs)
}
fn same_source_paragraph(a: &Paragraph, b: &Paragraph) -> bool {
    // Equal resolved fonts can still carry different unmodelled rPr/pPr data.
    a.source == b.source && a.runs == b.runs && same_paragraph(a, b)
}
fn same_runs(a: &[Run], b: &[Run]) -> bool {
    fn normalized(runs: &[Run]) -> Vec<(String, &crate::Font, Option<&crate::InlineRaster>)> {
        let mut result: Vec<(String, &crate::Font, Option<&crate::InlineRaster>)> = Vec::new();
        for run in runs.iter().filter(|r| !r.text.is_empty()) {
            if let Some(last) = result.last_mut()
                && *last.1 == run.font
                && last.2.is_none()
                && run.picture.is_none()
            {
                last.0.push_str(&run.text);
            } else {
                result.push((run.text.clone(), &run.font, run.picture.as_ref()));
            }
        }
        result
    }
    normalized(a) == normalized(b)
}
fn same_document(a: &Document, b: &Document) -> bool {
    a.content == b.content
        && a.page == b.page
        && a.tab_stop == b.tab_stop
        && a.compress_justification == b.compress_justification
        && a.paragraphs.len() == b.paragraphs.len()
        && a.paragraphs
            .iter()
            .zip(&b.paragraphs)
            .all(|(a, b)| same_paragraph(a, b))
        && a.footer.len() == b.footer.len()
        && a.footer
            .iter()
            .zip(&b.footer)
            .all(|(a, b)| same_paragraph(a, b))
}
fn text_xml(text: &str, prefix: &str) -> Result<String, String> {
    if text.chars().any(|c| {
        c == '\r'
            || (c.is_control() && c != '\n' && c != '\t')
            || matches!(c, '\u{fffe}' | '\u{ffff}')
    }) {
        return Err("Текст содержит символы, недопустимые при сохранении XML".into());
    }
    let mut result = String::new();
    let mut chunk = String::new();
    let flush = |chunk: &mut String, result: &mut String| {
        if !chunk.is_empty() {
            result.push_str(&format!(
                "<{prefix}:t xml:space=\"preserve\">{}</{prefix}:t>",
                escape(chunk)
            ));
            chunk.clear();
        }
    };
    for ch in text.chars() {
        match ch {
            '\t' | '\n' => {
                flush(&mut chunk, &mut result);
                result.push_str(&format!(
                    "<{prefix}:{}/>",
                    if ch == '\t' { "tab" } else { "br" }
                ));
            }
            _ => chunk.push(ch),
        }
    }
    flush(&mut chunk, &mut result);
    Ok(result)
}
fn paragraph_xml(
    source: &str,
    node: Node<'_, '_>,
    original: &Paragraph,
    current: &Paragraph,
    paragraphs: &[Node<'_, '_>],
    baseline: &Document,
    first: bool,
) -> Result<String, String> {
    let mut expected = original.clone();
    expected.style.page_break = current.style.page_break;
    if current.style != expected.style
        || current.style_id != original.style_id
        || current.mark_font != original.mark_font
        || current.list != original.list
    {
        return Err("Изменение свойств абзаца этим способом пока не поддерживается".into());
    }
    if edit_block(node).is_some() {
        return Err("Нельзя безопасно пересобрать абзац со служебными объектами".into());
    }
    let (mut result, prefix) = wrapper(node, "p", !first);
    if let Some(props) = child(node, "pPr") {
        if current.style.page_break == original.style.page_break {
            result.push_str(raw(source, props));
        } else {
            let (mut xml, pp) = wrapper(props, "pPr", false);
            let flag = format!(
                "<{pp}:pageBreakBefore {pp}:val=\"{}\"/>",
                u8::from(current.style.page_break)
            );
            let mut inserted = false;
            for n in props.children() {
                if is(n, "pageBreakBefore") {
                    continue;
                }
                // Respect CT_PPr schema order: pStyle, keepNext, keepLines, pageBreakBefore.
                if n.is_element()
                    && !inserted
                    && !["pStyle", "keepNext", "keepLines"]
                        .iter()
                        .any(|tag| is(n, tag))
                {
                    xml.push_str(&flag);
                    inserted = true;
                }
                xml.push_str(raw(source, n));
            }
            if !inserted {
                xml.push_str(&flag);
            }
            xml.push_str(&format!("</{pp}:pPr>"));
            result.push_str(&xml);
        }
    } else if current.style.page_break != original.style.page_break {
        result.push_str(&format!(
            "<{prefix}:pPr><{prefix}:pageBreakBefore {prefix}:val=\"{}\"/></{prefix}:pPr>",
            u8::from(current.style.page_break)
        ));
    }
    for run in current.runs.iter().filter(|r| !r.text.is_empty()) {
        if result.len() > MAX_XML {
            return Err("XML абзаца превышает 16 МиБ".into());
        }
        if let Some((p, r)) = run.source {
            let old = baseline
                .paragraphs
                .get(p)
                .ok_or("Не найдена основа фрагмента")?;
            if old.style_id != current.style_id {
                return Err(
                    "Объединение текста с разными стилями абзаца пока нельзя безопасно сохранить"
                        .into(),
                );
            }
            let original_run = old
                .runs
                .iter()
                .find(|v| v.source == Some((p, r)))
                .ok_or("Не найдено исходное оформление фрагмента")?;
            if original_run.font != run.font {
                return Err("Изменение шрифта пока не поддерживается экспортом".into());
            }
            if let Some(picture) = &run.picture {
                if original_run.picture.as_ref() != Some(picture)
                    || r < crate::blocks::DRAWING_SOURCE
                {
                    return Err("Не найдена исходная разметка рисунка".into());
                }
                let drawing = crate::blocks::drawings(paragraphs[p])
                    .nth(r - crate::blocks::DRAWING_SOURCE)
                    .ok_or("Не найден исходный узел рисунка")?;
                let source_run = drawing
                    .parent()
                    .filter(|n| is(*n, "r"))
                    .ok_or("Не найден фрагмент рисунка")?;
                let (mut xml, rp) = wrapper(source_run, "r", false);
                if let Some(props) = child(source_run, "rPr") {
                    xml.push_str(raw(source, props));
                }
                // A picture-only edited paragraph remains part of the text flow.
                xml.push_str(&format!("<{rp}:t/>"));
                xml.push_str(raw(source, drawing));
                xml.push_str(&format!("</{rp}:r>"));
                result.push_str(&xml);
                continue;
            }
            let source_run = paragraphs[p]
                .descendants()
                .filter(|n| is(*n, "r"))
                .nth(r)
                .ok_or("Не найден исходный XML фрагмента")?;
            let (mut xml, rp) = wrapper(source_run, "r", false);
            if let Some(props) = child(source_run, "rPr") {
                xml.push_str(raw(source, props));
            }
            xml.push_str(&text_xml(&run.text, &rp)?);
            xml.push_str(&format!("</{rp}:r>"));
            result.push_str(&xml);
        } else {
            // Typing into an empty paragraph inherits its paragraph-mark properties.
            if run.font != original.mark_font {
                return Err("Не найдена основа оформления нового текста".into());
            }
            result.push_str(&format!("<{prefix}:r>"));
            if let Some(props) = child(node, "pPr").and_then(|n| child(n, "rPr")) {
                let (mut xml, mark_prefix) = wrapper(props, "rPr", false);
                for n in props.children() {
                    xml.push_str(raw(source, n));
                }
                xml.push_str(&format!("</{mark_prefix}:rPr>"));
                result.push_str(&xml);
            }
            result.push_str(&text_xml(&run.text, &prefix)?);
            result.push_str(&format!("</{prefix}:r>"));
        }
    }
    result.push_str(&format!("</{prefix}:p>"));
    if result.len() > MAX_XML {
        return Err("XML абзаца превышает 16 МиБ".into());
    }
    Ok(result)
}
impl Document {
    pub fn to_docx(&self, page_count: Option<usize>) -> Result<Vec<u8>, String> {
        let modified = self.prepared_xml()?;
        if modified.is_none() && self.package.main.is_none() {
            return Ok(self.package.original.to_vec());
        }
        self.finish_export(modified, page_count)
    }
    fn prepared_xml(&self) -> Result<Option<(String, String)>, String> {
        let baseline = Document::from_package(self.package.clone())?;
        if same_document(self, &baseline)
            && self
                .paragraphs
                .iter()
                .zip(&baseline.paragraphs)
                .all(|(a, b)| same_source_paragraph(a, b))
        {
            return Ok(None);
        }
        if self.content.is_some() != baseline.content.is_some() {
            return Err("Структура блоков не совпадает с исходным документом".into());
        }
        if self
            .package
            .names()
            .any(|name| name.to_ascii_lowercase().starts_with("_xmlsignatures/"))
        {
            return Err("Изменение DOCX с цифровой подписью пока не поддерживается".into());
        }
        if self.page != baseline.page
            || self.tab_stop != baseline.tab_stop
            || self.compress_justification != baseline.compress_justification
            || self.footer.len() != baseline.footer.len()
            || !self
                .footer
                .iter()
                .zip(&baseline.footer)
                .all(|(a, b)| same_paragraph(a, b))
        {
            return Err(
                "Сохранение изменённых разделов или колонтитулов пока не поддерживается".into(),
            );
        }
        let main = relationship(&self.package, "_rels/.rels", "", "officeDocument")?
            .ok_or("Не найдена основная часть DOCX")?;
        let source = self.package.text(&main)?;
        let xml = parse_xml(&source).map_err(|e| e.to_string())?;
        if baseline.content.is_some() {
            let output = blocks::rewrite(self, &baseline, &source, &xml)?;
            return Ok(Some((main, output)));
        }
        let body = xml
            .descendants()
            .find(|n| is(*n, "body"))
            .ok_or("Нет тела документа")?;
        let paragraphs: Vec<_> = body.children().filter(|n| is(*n, "p")).collect();
        if paragraphs.len() != baseline.paragraphs.len() || paragraphs.is_empty() {
            return Err(
                "Сохранение вложенных или отсутствующих абзацев пока не поддерживается".into(),
            );
        }
        if body
            .children()
            .any(|n| n.is_comment() || (n.is_element() && !is(n, "p") && !is(n, "sectPr")))
        {
            return Err("Тело документа содержит неподдерживаемые объекты между абзацами".into());
        }
        for (index, p) in baseline
            .paragraphs
            .iter()
            .enumerate()
            .filter(|(_, p)| p.edit_block.is_some())
        {
            let matches: Vec<_> = self
                .paragraphs
                .iter()
                .filter(|p| p.source == Some(index))
                .collect();
            if matches.len() != 1 || !same_source_paragraph(matches[0], p) {
                return Err("Изменён или удалён абзац со служебными объектами".into());
            }
        }
        let first = paragraphs[0].range().start;
        let last = paragraphs.last().unwrap().range().end;
        // Preserve all unmodelled body content by refusing to discard it.
        let mut cursor = first;
        for p in &paragraphs {
            if !source[cursor..p.range().start].trim().is_empty() {
                return Err("Между абзацами есть неподдерживаемые данные".into());
            }
            cursor = p.range().end;
        }
        let mut output = source[..first].to_owned();
        let mut used = BTreeSet::new();
        for p in &self.paragraphs {
            if output.len() > MAX_XML {
                return Err("XML документа превышает 16 МиБ".into());
            }
            let index = p
                .source
                .ok_or("Новый абзац не связан с оформлением шаблона")?;
            let old = baseline
                .paragraphs
                .get(index)
                .ok_or("Не найдена основа абзаца")?;
            let first = used.insert(index);
            let mut expected = old.clone();
            expected.list_label = p.list_label.clone();
            if first && same_source_paragraph(&expected, p) {
                output.push_str(raw(&source, paragraphs[index]));
            } else {
                output.push_str(&paragraph_xml(
                    &source,
                    paragraphs[index],
                    old,
                    p,
                    &paragraphs,
                    &baseline,
                    first,
                )?);
            }
        }
        output.push_str(&source[last..]);
        if output.len() > MAX_XML {
            return Err("XML документа превышает 16 МиБ".into());
        }
        Ok(Some((main, output)))
    }
    fn finish_export(
        &self,
        modified: Option<(String, String)>,
        page_count: Option<usize>,
    ) -> Result<Vec<u8>, String> {
        let mut changes: BTreeMap<_, _> = modified
            .into_iter()
            .map(|(name, xml)| (name, xml.into_bytes()))
            .collect();
        if let Some(count) = page_count.filter(|count| *count > 0 && *count <= 100000)
            && self.package.contains("docProps/app.xml")
        {
            let source = self.package.text("docProps/app.xml")?;
            let xml = parse_xml(&source).map_err(|e| e.to_string())?;
            if let Some(pages) = xml
                .descendants()
                .find(|n| n.is_element() && n.tag_name().name() == "Pages")
            {
                let text = pages
                    .children()
                    .find(|n| n.is_text())
                    .ok_or("Неподдерживаемый счётчик страниц")?;
                let mut updated = source.clone();
                updated.replace_range(text.range(), &count.to_string());
                changes.insert("docProps/app.xml".into(), updated.into_bytes());
            }
        }
        let bytes = self.package.replace(&changes)?;
        let reopened = Document::from_bytes(bytes.clone())?;
        if !same_document(self, &reopened) {
            return Err("Проверка сохранения не пройдена: текст или оформление отличаются. Файл не записан.".into());
        }
        Ok(bytes)
    }
    /// Publish a complete new file without ever replacing an existing path.
    pub fn save_new(&self, path: &Path, page_count: Option<usize>) -> Result<(), String> {
        let bytes = self.to_docx(page_count)?;
        write_new(path, &bytes).map_err(|e| {
            format!("Не удалось сохранить копию: {e}. Существующие файлы не изменены.")
        })
    }
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
struct TempPath(PathBuf);
impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn write_new(destination: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if destination.file_name().is_none() {
        return Err(std::io::Error::other("Укажите имя нового файла DOCX"));
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for _ in 0..128 {
        let count = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".paperik-{}-{stamp:x}-{count:x}.tmp",
            std::process::id()
        ));
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let temporary = TempPath(path);
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        publish_new(&temporary.0, destination)?;
        return Ok(());
    }
    Err(std::io::Error::other("Не удалось создать временный файл"))
}
#[cfg(windows)]
fn publish_new(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    let wide = |path: &Path| -> std::io::Result<Vec<u16>> {
        let value: Vec<_> = path.as_os_str().encode_wide().collect();
        if value.contains(&0) {
            return Err(std::io::Error::other("Нулевой символ в пути"));
        }
        Ok(value.into_iter().chain(Some(0)).collect())
    };
    let source = wide(source)?;
    let destination = wide(destination)?;
    // No MOVEFILE_REPLACE_EXISTING: Windows checks destination existence atomically.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
#[cfg(not(windows))]
fn publish_new(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::hard_link(source, destination)
}

#[cfg(test)]
mod tests;
