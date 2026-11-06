use super::*;
use crate::package::{PictureParts, owned_picture_name};
use std::{ops::Range, sync::Arc};

const WP: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const PIC: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const RELS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const CT: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
const TYPES: &str = "[Content_Types].xml";

pub enum PictureAction {
    Insert {
        bytes: Arc<Vec<u8>>,
        size: [u32; 2],
    },
    Replace {
        bytes: Arc<Vec<u8>>,
    },
    Delete,
    /// Width and height in twips.
    Resize {
        size: [u32; 2],
    },
}

pub struct PictureEdit {
    pub paragraph: usize,
    /// Text offset for inline insertion or the first unit of an existing object.
    pub offset: Option<u32>,
    pub action: PictureAction,
}

impl Document {
    pub(crate) fn edit_picture(
        &self,
        edit: PictureEdit,
    ) -> Result<(Document, crate::editor::Position), String> {
        let prepared = self.prepared_xml()?;
        let (main, mut source) = if let Some((main, xml)) = prepared {
            let materialized =
                Document::from_package(self.package.with_main(main.clone(), xml.clone())?)?;
            if !same_document(self, &materialized) {
                return Err("Текущие правки текста нельзя перенести в структуру документа".into());
            }
            (main, xml)
        } else {
            let main = relationship(&self.package, "_rels/.rels", "", "officeDocument")?
                .ok_or("Не найдена основная часть DOCX")?;
            let source = self.package.text(&main)?;
            (main, source)
        };
        let rel_path = crate::part_relationships(&main);
        let mut parts = self
            .package
            .main
            .as_ref()
            .map_or_else(BTreeMap::new, |p| p.parts.clone());
        let xml = parse_xml(&source).map_err(|e| e.to_string())?;
        let body = xml
            .descendants()
            .find(|n| is(*n, "body"))
            .ok_or("Нет тела документа")?;
        let paragraphs: Vec<_> = xml
            .descendants()
            .filter(|n| is(*n, "p") && !n.ancestors().any(|a| is(a, "txbxContent")))
            .collect();
        let paragraph = paragraphs
            .get(edit.paragraph)
            .copied()
            .ok_or("Не найден абзац рисунка")?;
        let direct = |node: &Node<'_, '_>| node.parent() == Some(body);

        let (mut patches, focus) = match edit.action {
            PictureAction::Insert { bytes, size } => {
                if body.tag_name().namespace() != Some(W) {
                    return Err(
                        "Вставка рисунков в DOCX формата Strict пока не поддерживается".into(),
                    );
                }
                let doc_id = drawing_id(&xml)?;
                let emu = dimensions(size)?;
                let id = add_media(&self.package, &rel_path, &mut parts, bytes)?;
                let markup = picture_markup(doc_id, &id, emu);
                if let Some(offset) = edit.offset {
                    let original = &self.paragraphs[edit.paragraph];
                    if original.edit_block.is_some() || original.page_field {
                        return Err("Этот абзац защищён от вставки в строку".into());
                    }
                    let patch =
                        insert_inline(&source, paragraph, &original.text(), offset, &markup)?;
                    (vec![patch], (edit.paragraph, offset + 1))
                } else {
                    let anchor = paragraph
                        .ancestors()
                        .find(direct)
                        .ok_or("Не найден блок для вставки")?;
                    if !is(anchor, "p") && !is(anchor, "tbl") {
                        return Err("Выберите обычный абзац или таблицу для вставки рисунка".into());
                    }
                    let end = anchor.range().end;
                    let focus = paragraphs.partition_point(|p| p.range().end <= end);
                    let paragraph = format!(
                        r#"<w:p xmlns:w="{W}"><w:pPr><w:jc w:val="center"/></w:pPr><w:r>{markup}</w:r></w:p>"#
                    );
                    (vec![(end..end, paragraph)], (focus, 0))
                }
            }
            action => {
                let ordinal = if let Some(offset) = edit.offset {
                    self.paragraphs[edit.paragraph]
                        .inline_pictures()
                        .position(|(at, _, _)| at == offset)
                        .ok_or("Выберите встроенный рисунок")?
                } else if crate::blocks::standalone(paragraph) {
                    0
                } else {
                    return Err("Выберите рисунок в строке или отдельный рисунок".into());
                };
                let drawing = crate::blocks::drawings(paragraph)
                    .nth(ordinal)
                    .ok_or("Выберите встроенный рисунок")?;
                let inline = drawing
                    .children()
                    .find(|n| n.has_tag_name((WP, "inline")))
                    .ok_or("Плавающие рисунки пока не редактируются")?;
                let picture = inline
                    .descendants()
                    .find(|n| n.has_tag_name((PIC, "pic")))
                    .ok_or("Выберите встроенный растровый рисунок")?;
                let patches = match action {
                    PictureAction::Delete => vec![(delete_range(drawing), String::new())],
                    PictureAction::Resize { size } => resize(inline, picture, dimensions(size)?)?,
                    PictureAction::Replace { bytes } => {
                        let blip = picture
                            .descendants()
                            .find(|n| n.has_tag_name((A, "blip")))
                            .ok_or("Не найден источник изображения")?;
                        let embed = blip
                            .attribute_node((R, "embed"))
                            .ok_or("Нет связи встроенного изображения")?;
                        if blip.attribute((R, "link")).is_some()
                            || blip.descendants().any(|n| {
                                n.attributes()
                                    .any(|a| a.namespace() == Some(R) && n != blip)
                            })
                        {
                            return Err(
                                "У рисунка есть дополнительные ссылки на изображение".into()
                            );
                        }
                        let id = add_media(&self.package, &rel_path, &mut parts, bytes)?;
                        vec![(embed.range_value(), id)]
                    }
                    PictureAction::Insert { .. } => unreachable!(),
                };
                (patches, (edit.paragraph, edit.offset.unwrap_or(0)))
            }
        };
        patches.sort_unstable_by_key(|(range, _)| std::cmp::Reverse(range.start));
        for (range, replacement) in patches {
            source.replace_range(range, &replacement);
        }
        prune_added_media(&source, &rel_path, &mut parts)?;
        let document =
            Document::from_package(self.package.with_picture_parts(main, source, parts)?)?;
        Ok((
            document,
            crate::editor::Position {
                paragraph: focus.0,
                offset: focus.1,
            },
        ))
    }
}

fn insert_inline(
    source: &str,
    paragraph: Node<'_, '_>,
    text: &str,
    offset: u32,
    drawing: &str,
) -> Result<(Range<usize>, String), String> {
    use crate::editor::{snap, utf16_len};
    if offset > utf16_len(text) || snap(text, offset) != offset {
        return Err("Позиция вставки не совпадает с границей символа".into());
    }
    let mut cursor = 0;
    let mut last = None;
    for node in paragraph
        .children()
        .filter(|n| is(*n, "r"))
        .flat_map(|r| r.children())
    {
        let value = node.text().unwrap_or("");
        let length = if is(node, "t") {
            utf16_len(value)
        } else if ["tab", "br", "cr", "noBreakHyphen", "drawing"]
            .iter()
            .any(|name| is(node, name))
        {
            1
        } else {
            0
        };
        if length == 0 {
            continue;
        }
        let range = node.range();
        if offset == cursor {
            return Ok((range.start..range.start, drawing.into()));
        }
        if offset < cursor + length {
            let mut units = cursor;
            let at = value
                .char_indices()
                .find_map(|(byte, c)| {
                    let found = (units == offset).then_some(byte);
                    units += c.len_utf16() as u32;
                    found
                })
                .ok_or("Нельзя разделить фрагмент в позиции вставки")?;
            let (left, right) = value.split_at(at);
            let text = |value| {
                format!(
                    r#"<w:t xmlns:w="{W}" xml:space="preserve">{}</w:t>"#,
                    escape(value)
                )
            };
            return Ok((range, format!("{}{drawing}{}", text(left), text(right))));
        }
        cursor += length;
        last = Some(range.end);
    }
    if cursor != offset {
        return Err("Не найдена позиция рисунка в разметке".into());
    }
    if let Some(end) = last {
        Ok((end..end, drawing.into()))
    } else {
        let markup = format!(r#"<w:r xmlns:w="{W}"><w:t/>{drawing}</w:r>"#);
        append_child(source, paragraph, &markup)
    }
}

fn delete_range(drawing: Node<'_, '_>) -> Range<usize> {
    if let Some(run) = drawing.parent().filter(|r| is(*r, "r"))
        && run.children().all(|n| {
            n == drawing
                || is(n, "rPr")
                || (is(n, "t") && n.text().is_none_or(str::is_empty))
                || (n.is_text() && n.text().is_none_or(|s| s.trim().is_empty()))
        })
    {
        return run.range();
    }
    drawing.range()
}

fn dimensions(size: [u32; 2]) -> Result<[u32; 2], String> {
    if size.into_iter().any(|n| !(1..=150_000).contains(&n)) {
        return Err("Недопустимый размер рисунка".into());
    }
    Ok(size.map(|n| n * 635))
}

fn resize(
    inline: Node<'_, '_>,
    picture: Node<'_, '_>,
    [width, height]: [u32; 2],
) -> Result<Vec<(Range<usize>, String)>, String> {
    let outer = inline
        .children()
        .find(|n| n.has_tag_name((WP, "extent")))
        .ok_or("Не найдены размеры рисунка")?;
    let inner = picture
        .children()
        .find(|n| n.has_tag_name((PIC, "spPr")))
        .and_then(|p| p.children().find(|n| n.has_tag_name((A, "xfrm"))))
        .and_then(|p| p.children().find(|n| n.has_tag_name((A, "ext"))))
        .ok_or("Не найден размер фигуры рисунка")?;
    [outer, inner]
        .into_iter()
        .flat_map(|node| {
            [("cx", width), ("cy", height)].map(move |(key, value)| {
                let range = node
                    .attribute_node(key)
                    .ok_or("Не найден размер рисунка")?
                    .range_value();
                Ok((range, value.to_string()))
            })
        })
        .collect()
}

fn drawing_id(xml: &roxmltree::Document<'_>) -> Result<u32, String> {
    let used: BTreeSet<u32> = xml
        .descendants()
        .filter(|n| n.has_tag_name((WP, "docPr")))
        .filter_map(|n| n.attribute("id")?.parse().ok())
        .collect();
    (1..=u32::MAX)
        .find(|id| !used.contains(id))
        .ok_or_else(|| "Нет свободного идентификатора рисунка".into())
}

fn picture_markup(id: u32, relation: &str, [cx, cy]: [u32; 2]) -> String {
    format!(
        concat!(
            r#"<w:drawing xmlns:w="{w}">"#,
            r#"<wp:inline xmlns:wp="{wp}" xmlns:a="{a}" xmlns:pic="{pic}" xmlns:r="{r}">"#,
            r#"<wp:extent cx="{cx}" cy="{cy}"/><wp:docPr id="{id}" name="Picture {id}"/>"#,
            r#"<wp:cNvGraphicFramePr><a:graphicFrameLocks noChangeAspect="1"/></wp:cNvGraphicFramePr>"#,
            r#"<a:graphic><a:graphicData uri="{pic}"><pic:pic>"#,
            r#"<pic:nvPicPr><pic:cNvPr id="0" name="Picture {id}"/><pic:cNvPicPr/></pic:nvPicPr>"#,
            r#"<pic:blipFill><a:blip r:embed="{relation}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>"#,
            r#"<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm>"#,
            r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr>"#,
            r#"</pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>"#,
        ),
        w = W,
        wp = WP,
        a = A,
        pic = PIC,
        r = R,
        cx = cx,
        cy = cy,
        id = id,
        relation = relation
    )
}

fn add_media(
    package: &crate::Package,
    rel_path: &str,
    parts: &mut PictureParts,
    bytes: Arc<Vec<u8>>,
) -> Result<String, String> {
    let (extension, mime) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        ("png", "image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        ("jpg", "image/jpeg")
    } else {
        return Err("Поддерживаются рисунки PNG и JPEG".into());
    };
    if bytes.len() > crate::package::MAX_PART {
        return Err("Рисунок превышает предел 16 МиБ".into());
    }
    let mut relations = if package.contains(rel_path) {
        package.text(rel_path)?
    } else {
        format!(r#"<Relationships xmlns="{RELS}"/>"#)
    };
    let mut types = package.text(TYPES)?;
    let rel_doc = parse_xml(&relations).map_err(|e| e.to_string())?;
    let type_doc = parse_xml(&types).map_err(|e| e.to_string())?;
    let rel_root = rel_doc.root_element();
    let types_root = type_doc.root_element();
    if !rel_root.has_tag_name((RELS, "Relationships")) || !types_root.has_tag_name((CT, "Types")) {
        return Err("Некорректные связи или типы содержимого DOCX".into());
    }
    let (id, path) = (1..=u32::MAX)
        .find_map(|n| {
            let id = format!("rIdPaperikImage{n}");
            let path = format!("word/media/paperik-image-{n}.{extension}");
            let part_name = format!("/{path}");
            (!package.names().any(|v| v.eq_ignore_ascii_case(&path))
                && !rel_root.children().any(|n| n.attribute("Id") == Some(&id))
                && !types_root.children().any(|n| {
                    n.attribute("PartName")
                        .is_some_and(|v| v.eq_ignore_ascii_case(&part_name))
                }))
            .then_some((id, path))
        })
        .ok_or("Нет свободного адреса рисунка")?;
    let relation =
        format!(r#"<Relationship xmlns="{RELS}" Id="{id}" Type="{R}/image" Target="/{path}"/>"#);
    let content_type =
        format!(r#"<Override xmlns="{CT}" PartName="/{path}" ContentType="{mime}"/>"#);
    let rel_patch = append_child(&relations, rel_root, &relation)?;
    let type_patch = append_child(&types, types_root, &content_type)?;
    relations.replace_range(rel_patch.0, &rel_patch.1);
    types.replace_range(type_patch.0, &type_patch.1);
    parts.insert(rel_path.into(), Arc::new(relations.into_bytes()));
    parts.insert(TYPES.into(), Arc::new(types.into_bytes()));
    parts.insert(path, bytes);
    Ok(id)
}

fn append_child(
    xml: &str,
    node: Node<'_, '_>,
    markup: &str,
) -> Result<(Range<usize>, String), String> {
    let range = node.range();
    let end = range.end;
    if xml[range.clone()].ends_with("/>") {
        let name = xml[range.start + 1..end - 2]
            .split(|c: char| c.is_whitespace())
            .next()
            .ok_or("Не найден корневой элемент части")?;
        Ok((end - 2..end, format!(">{markup}</{name}>")))
    } else {
        let end = xml[range]
            .rfind("</")
            .map(|start| node.range().start + start)
            .ok_or("Не найден конец корневого элемента части")?;
        Ok((end..end, markup.into()))
    }
}

fn prune_added_media(source: &str, rel_path: &str, parts: &mut PictureParts) -> Result<(), String> {
    let Some(rels) = parts.get(rel_path) else {
        return Ok(());
    };
    let text = std::str::from_utf8(rels).map_err(|e| e.to_string())?;
    let relationships = parse_xml(text).map_err(|e| e.to_string())?;
    let doc = parse_xml(source).map_err(|e| e.to_string())?;
    let used: BTreeSet<_> = doc
        .descendants()
        .flat_map(|n| n.attributes())
        .map(|a| a.value())
        .collect();
    let mut garbage = BTreeSet::new();
    let mut remove_rels = Vec::new();
    for node in relationships
        .root_element()
        .children()
        .filter(|n| n.has_tag_name((RELS, "Relationship")))
    {
        let Some(path) = node
            .attribute("Target")
            .and_then(|target| target.strip_prefix('/'))
        else {
            continue;
        };
        if owned_picture_name(path)
            && parts.contains_key(path)
            && node.attribute("Id").is_some_and(|id| !used.contains(id))
        {
            garbage.insert(path.to_owned());
            remove_rels.push(node.range());
        }
    }
    if garbage.is_empty() {
        return Ok(());
    }
    let mut rels = text.to_owned();
    for range in remove_rels.into_iter().rev() {
        rels.replace_range(range, "");
    }
    let types = parts.get(TYPES).ok_or("Нет типов рисунков")?;
    let mut types = String::from_utf8(types.as_ref().clone()).map_err(|e| e.to_string())?;
    let xml = parse_xml(&types).map_err(|e| e.to_string())?;
    let ranges: Vec<_> = xml
        .root_element()
        .children()
        .filter(|n| {
            n.has_tag_name((CT, "Override"))
                && n.attribute("PartName")
                    .and_then(|s| s.strip_prefix('/'))
                    .is_some_and(|path| garbage.contains(path))
        })
        .map(|n| n.range())
        .collect();
    for range in ranges.into_iter().rev() {
        types.replace_range(range, "");
    }
    parts.insert(rel_path.into(), Arc::new(rels.into_bytes()));
    parts.insert(TYPES.into(), Arc::new(types.into_bytes()));
    for path in garbage {
        parts.remove(&path);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
