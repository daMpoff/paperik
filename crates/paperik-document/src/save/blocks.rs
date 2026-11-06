//! Rewrite text scopes in place, preserving table/cell/drawing XML around them.
use super::*;

pub(super) fn rewrite(
    current: &Document,
    baseline: &Document,
    source: &str,
    xml: &crate::Xml<'_>,
) -> Result<String, String> {
    let old = baseline
        .content
        .as_ref()
        .ok_or("Нет исходной структуры блоков")?;
    let rebound = old.rebind(&baseline.paragraphs, &current.paragraphs)?;
    if current.content.as_ref() != Some(&rebound) {
        return Err(
            "Изменение сетки, свойств таблицы или рисунка пока нельзя экспортировать".into(),
        );
    }
    let old_scopes = old.scopes(baseline.paragraphs.len())?;
    let new_scopes = rebound.scopes(current.paragraphs.len())?;
    let paragraphs: Vec<_> = xml
        .descendants()
        .filter(|n| is(*n, "p") && !n.ancestors().any(|a| is(a, "txbxContent")))
        .collect();
    if paragraphs.len() != baseline.paragraphs.len() {
        return Err("Исходные абзацы не совпадают со структурой XML".into());
    }
    let mut output = String::new();
    let mut cursor = 0;
    for (old, new) in old_scopes.iter().zip(&new_scopes) {
        if old.paragraphs.len() == new.paragraphs.len()
            && old.paragraphs.iter().zip(&new.paragraphs).all(|(a, b)| {
                same_source_paragraph(&baseline.paragraphs[*a], &current.paragraphs[*b])
            })
        {
            continue;
        }
        if old.picture {
            return Err("Нельзя пересобрать рисунок текстовой правкой".into());
        }
        let first = paragraphs[old.paragraphs[0]];
        let last = paragraphs[*old.paragraphs.last().unwrap()];
        let mut end = first.range().start;
        for index in &old.paragraphs {
            let p = paragraphs[*index];
            if p.parent() != first.parent() || !source[end..p.range().start].trim().is_empty() {
                return Err("Между абзацами области есть неподдерживаемые данные".into());
            }
            end = p.range().end;
        }
        if first.range().start < cursor {
            return Err("Области XML пересекаются".into());
        }
        output.push_str(&source[cursor..first.range().start]);
        let mut used = BTreeSet::new();
        for index in &new.paragraphs {
            let p = &current.paragraphs[*index];
            let origin = p.source.ok_or("Нет исходного оформления абзаца")?;
            let original = baseline
                .paragraphs
                .get(origin)
                .ok_or("Исходный абзац не найден")?;
            let first = used.insert(origin);
            let mut expected = original.clone();
            expected.list_label = p.list_label.clone();
            if first && same_source_paragraph(&expected, p) {
                output.push_str(raw(source, paragraphs[origin]));
            } else {
                output.push_str(&paragraph_xml(
                    source,
                    paragraphs[origin],
                    original,
                    p,
                    &paragraphs,
                    baseline,
                    first,
                )?);
            }
            if output.len() > MAX_XML {
                return Err("XML документа превышает 16 МиБ".into());
            }
        }
        cursor = last.range().end;
    }
    output.push_str(&source[cursor..]);
    if output.len() > MAX_XML {
        return Err("XML документа превышает 16 МиБ".into());
    }
    Ok(output)
}

#[cfg(test)]
mod tests;
