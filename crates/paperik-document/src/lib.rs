//! Paperik's document model and bounded DOCX reader. No office engine is used.
mod package;

pub use package::Package;
use roxmltree::{Document as Xml, Node};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const WS: &str = "http://purl.oclc.org/ooxml/wordprocessingml/main";

fn parse_xml(source: &str) -> Result<Xml<'_>, roxmltree::Error> {
    Xml::parse_with_options(
        source,
        roxmltree::ParsingOptions {
            nodes_limit: 500_000,
            ..Default::default()
        },
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct Font {
    pub family: String,
    pub points: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub kerning_from: Option<f32>,
    pub ligatures: u8,
    pub color: [u8; 3],
}
impl Default for Font {
    fn default() -> Self {
        Self {
            family: "Times New Roman".into(),
            points: 12.0,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            kerning_from: None,
            ligatures: 0,
            color: [0, 0, 0],
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Alignment {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineSpacing {
    Multiple(f32),
    Exact(f32),
    AtLeast(f32),
}
#[derive(Clone, Debug, PartialEq)]
pub struct ParagraphStyle {
    pub alignment: Alignment,
    pub before: f32,
    pub after: f32,
    pub left: f32,
    pub right: f32,
    pub first_line: f32,
    pub line: LineSpacing,
    pub keep_next: bool,
    pub keep_lines: bool,
    pub page_break: bool,
    pub widow: bool,
    pub contextual: bool,
    pub tabs: Vec<(f32, String)>,
}
impl Default for ParagraphStyle {
    fn default() -> Self {
        Self {
            alignment: Alignment::Left,
            before: 0.0,
            after: 0.0,
            left: 0.0,
            right: 0.0,
            first_line: 0.0,
            line: LineSpacing::Multiple(1.0),
            keep_next: false,
            keep_lines: false,
            page_break: false,
            widow: true,
            contextual: false,
            tabs: Vec::new(),
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub text: String,
    pub font: Font,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Paragraph {
    pub runs: Vec<Run>,
    pub style: ParagraphStyle,
    pub mark_font: Font,
    pub style_id: String,
    pub list_label: Option<String>,
    pub page_field: bool,
}
impl Paragraph {
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}
#[derive(Clone, Debug)]
pub struct Page {
    pub width: f32,
    pub height: f32,
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
    pub footer_distance: f32,
    pub different_first: bool,
}
impl Default for Page {
    fn default() -> Self {
        Self {
            width: 793.7,
            height: 1122.5,
            left: 96.0,
            right: 96.0,
            top: 96.0,
            bottom: 96.0,
            footer_distance: 48.0,
            different_first: false,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Document {
    pub package: Package,
    pub paragraphs: Vec<Paragraph>,
    pub footer: Vec<Paragraph>,
    pub page: Page,
    pub warnings: Vec<String>,
    pub tab_stop: f32,
    pub saved_page_count: Option<usize>,
    pub compress_justification: bool,
}

fn is(node: Node<'_, '_>, local: &str) -> bool {
    node.is_element()
        && node.tag_name().name() == local
        && matches!(node.tag_name().namespace(), Some(W | WS))
}
fn child<'a, 'i>(node: Node<'a, 'i>, local: &str) -> Option<Node<'a, 'i>> {
    node.children().find(|n| is(*n, local))
}
fn attr<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attribute((W, name))
        .or_else(|| node.attribute((WS, name)))
}
fn val<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    child(node, name).and_then(|n| attr(n, "val"))
}
fn number(value: Option<&str>) -> Option<f32> {
    value
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|v| v.is_finite() && v.abs() <= 1_000_000.0)
}
fn enabled(node: Node<'_, '_>) -> bool {
    !matches!(attr(node, "val"), Some("0" | "false" | "off"))
}

fn font_properties(mut font: Font, properties: Option<Node<'_, '_>>, theme: &str) -> Font {
    let Some(properties) = properties else {
        return font;
    };
    if let Some(fonts) = child(properties, "rFonts") {
        if let Some(name) = attr(fonts, "hAnsi").or_else(|| attr(fonts, "ascii")) {
            font.family = name.into();
        } else if attr(fonts, "hAnsiTheme")
            .or_else(|| attr(fonts, "asciiTheme"))
            .is_some()
            && !theme.is_empty()
        {
            font.family = theme.into();
        }
    }
    if let Some(size) = number(val(properties, "sz")) {
        font.points = size / 2.0;
    }
    if let Some(size) = number(val(properties, "kern")) {
        font.kerning_from = Some(size / 2.0);
    }
    if let Some(node) = properties.children().find(|n| {
        n.tag_name().name() == "ligatures"
            && n.tag_name().namespace()
                == Some("http://schemas.microsoft.com/office/word/2010/wordml")
    }) {
        let value = node
            .attributes()
            .find(|a| a.name() == "val")
            .map_or("none", |a| a.value());
        font.ligatures = match value {
            "standard" => 1,
            "contextual" => 2,
            "standardContextual" => 3,
            "discretionary" => 4,
            "historical" => 8,
            "all" => 15,
            _ => 0,
        };
    }
    for (name, value) in [
        ("b", &mut font.bold),
        ("i", &mut font.italic),
        ("strike", &mut font.strike),
    ] {
        if let Some(node) = child(properties, name) {
            *value = enabled(node);
        }
    }
    if let Some(node) = child(properties, "u") {
        font.underline = !matches!(attr(node, "val"), Some("none" | "0" | "false"));
    }
    if let Some(color) = val(properties, "color").filter(|s| s.len() == 6 && s.is_ascii())
        && let Ok(value) = u32::from_str_radix(color, 16)
    {
        font.color = [(value >> 16) as u8, (value >> 8) as u8, value as u8];
    }
    font
}

fn paragraph_properties(
    mut style: ParagraphStyle,
    properties: Option<Node<'_, '_>>,
) -> ParagraphStyle {
    let Some(properties) = properties else {
        return style;
    };
    if let Some(value) = val(properties, "jc") {
        style.alignment = match value {
            "center" => Alignment::Center,
            "right" | "end" => Alignment::Right,
            "both" | "distribute" => Alignment::Justify,
            _ => Alignment::Left,
        };
    }
    if let Some(spacing) = child(properties, "spacing") {
        if let Some(v) = number(attr(spacing, "before")) {
            style.before = v / 15.0;
        }
        if let Some(v) = number(attr(spacing, "after")) {
            style.after = v / 15.0;
        }
        if let Some(v) = number(attr(spacing, "line")) {
            style.line = match attr(spacing, "lineRule") {
                Some("exact") => LineSpacing::Exact(v / 15.0),
                Some("atLeast") => LineSpacing::AtLeast(v / 15.0),
                _ => LineSpacing::Multiple(v / 240.0),
            };
        }
    }
    if let Some(ind) = child(properties, "ind") {
        if let Some(v) = number(attr(ind, "left").or_else(|| attr(ind, "start"))) {
            style.left = v / 15.0;
        }
        if let Some(v) = number(attr(ind, "right").or_else(|| attr(ind, "end"))) {
            style.right = v / 15.0;
        }
        if let Some(v) = number(attr(ind, "hanging")) {
            style.first_line = -v / 15.0;
        }
        if let Some(v) = number(attr(ind, "firstLine")) {
            style.first_line = v / 15.0;
        }
    }
    for (name, field) in [
        ("keepNext", &mut style.keep_next),
        ("keepLines", &mut style.keep_lines),
        ("pageBreakBefore", &mut style.page_break),
        ("widowControl", &mut style.widow),
        ("contextualSpacing", &mut style.contextual),
    ] {
        if let Some(node) = child(properties, name) {
            *field = enabled(node);
        }
    }
    if let Some(tabs) = child(properties, "tabs") {
        for tab in tabs.children().filter(|n| is(*n, "tab")) {
            if let Some(position) = number(attr(tab, "pos")) {
                style.tabs.retain(|(p, _)| *p != position / 15.0);
                if attr(tab, "val") != Some("clear") {
                    style
                        .tabs
                        .push((position / 15.0, attr(tab, "val").unwrap_or("left").into()));
                }
            }
        }
        style.tabs.sort_by(|a, b| a.0.total_cmp(&b.0));
    }
    style
}

struct Styles<'a> {
    xml: &'a Xml<'a>,
    theme: &'a str,
    default_id: String,
}
impl Styles<'_> {
    fn resolve(&self, id: &str) -> Result<(Font, ParagraphStyle), String> {
        let defaults = self.xml.descendants().find(|n| is(*n, "docDefaults"));
        let mut font = font_properties(
            Font::default(),
            defaults
                .and_then(|n| child(n, "rPrDefault"))
                .and_then(|n| child(n, "rPr")),
            self.theme,
        );
        let mut paragraph = paragraph_properties(
            ParagraphStyle::default(),
            defaults
                .and_then(|n| child(n, "pPrDefault"))
                .and_then(|n| child(n, "pPr")),
        );
        let mut chain = Vec::new();
        let mut visited = BTreeSet::new();
        let mut current = id;
        while !current.is_empty() {
            if !visited.insert(current) || chain.len() > 64 {
                return Err("Цикл наследования стилей".into());
            }
            let Some(style) = self
                .xml
                .descendants()
                .find(|n| is(*n, "style") && attr(*n, "styleId") == Some(current))
            else {
                return Err(format!("Не найден стиль {current}"));
            };
            chain.push(style);
            current = val(style, "basedOn").unwrap_or("");
        }
        for style in chain.into_iter().rev() {
            font = font_properties(font, child(style, "rPr"), self.theme);
            paragraph = paragraph_properties(paragraph, child(style, "pPr"));
        }
        Ok((font, paragraph))
    }
    fn character(&self, font: Font, id: &str) -> Result<Font, String> {
        let mut chain = Vec::new();
        let mut visited = BTreeSet::new();
        let mut current = id;
        while !current.is_empty() {
            if !visited.insert(current) || chain.len() > 64 {
                return Err("Цикл символьных стилей".into());
            }
            let node = self
                .xml
                .descendants()
                .find(|n| is(*n, "style") && attr(*n, "styleId") == Some(current))
                .ok_or("Не найден символьный стиль")?;
            chain.push(node);
            current = val(node, "basedOn").unwrap_or("");
        }
        Ok(chain.into_iter().rev().fold(font, |font, node| {
            font_properties(font, child(node, "rPr"), self.theme)
        }))
    }
}

fn resolve_target(base: &str, target: &str) -> Result<String, String> {
    if target.contains([':', '\\', '\0', '?', '#', '%']) {
        return Err("Неподдерживаемый адрес части DOCX".into());
    }
    let mut components: Vec<_> = if target.starts_with('/') {
        Vec::new()
    } else {
        base.rsplit_once('/')
            .map(|(p, _)| p.split('/').collect())
            .unwrap_or_default()
    };
    for component in target.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop().ok_or("Адрес выходит за пакет")?;
            }
            other => components.push(other),
        }
    }
    Ok(components.join("/"))
}
fn relationship(
    package: &Package,
    rel_path: &str,
    base: &str,
    kind: &str,
) -> Result<Option<String>, String> {
    if !package.contains(rel_path) {
        return Ok(None);
    }
    let source = package.text(rel_path)?;
    let xml = parse_xml(&source).map_err(|e| format!("Relationships: {e}"))?;
    let Some(node) = xml.descendants().find(|n| {
        n.has_tag_name("Relationship")
            && n.attribute("Type")
                .is_some_and(|t| t.ends_with(&format!("/{kind}")))
    }) else {
        return Ok(None);
    };
    if node.attribute("TargetMode") == Some("External") {
        return Err("Внешняя основная часть не поддерживается".into());
    }
    Ok(Some(resolve_target(
        base,
        node.attribute("Target").ok_or("Нет адреса части")?,
    )?))
}

fn read_paragraphs(
    xml: &Xml<'_>,
    styles: &Styles<'_>,
    numbering: Option<&Xml<'_>>,
    warnings: &mut BTreeSet<String>,
) -> Result<Vec<Paragraph>, String> {
    let mut result = Vec::new();
    let mut counters: BTreeMap<(String, String), u32> = BTreeMap::new();
    for node in xml
        .descendants()
        .filter(|n| is(*n, "p") && !n.ancestors().any(|a| is(a, "tbl") || is(a, "txbxContent")))
    {
        let props = child(node, "pPr");
        let style_id = props
            .and_then(|p| val(p, "pStyle"))
            .unwrap_or(&styles.default_id)
            .to_owned();
        let (base_font, base_paragraph) = styles.resolve(&style_id)?;
        let mut paragraph_style = base_paragraph;
        let mut label = None;
        if let Some(num) = props.and_then(|p| child(p, "numPr")) {
            let num_id = val(num, "numId").unwrap_or("0");
            let level = val(num, "ilvl").unwrap_or("0");
            if num_id != "0"
                && let Some(numbering) = numbering
            {
                let abstract_id = numbering
                    .descendants()
                    .find(|n| is(*n, "num") && attr(*n, "numId") == Some(num_id))
                    .and_then(|n| val(n, "abstractNumId"));
                let definition = abstract_id
                    .and_then(|id| {
                        numbering.descendants().find(|n| {
                            is(*n, "abstractNum") && attr(*n, "abstractNumId") == Some(id)
                        })
                    })
                    .and_then(|n| {
                        n.children()
                            .find(|n| is(*n, "lvl") && attr(*n, "ilvl") == Some(level))
                    });
                if let Some(definition) = definition {
                    paragraph_style =
                        paragraph_properties(paragraph_style, child(definition, "pPr"));
                    let start = val(definition, "start")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(1);
                    let count = counters
                        .entry((num_id.into(), level.into()))
                        .or_insert(start);
                    let text = val(definition, "lvlText").unwrap_or("•");
                    label = Some(match val(definition, "numFmt").unwrap_or("bullet") {
                        "bullet" => {
                            if text.contains('\u{f0b7}') {
                                "•".into()
                            } else {
                                text.into()
                            }
                        }
                        "decimal" => text.replace(
                            &format!("%{}", level.parse::<u32>().unwrap_or(0) + 1),
                            &count.to_string(),
                        ),
                        other => {
                            warnings.insert(format!("Формат списка {other} отображён приближённо"));
                            text.into()
                        }
                    });
                    *count += 1;
                } else {
                    warnings.insert("Не найдены параметры списка".into());
                }
            }
        }
        paragraph_style = paragraph_properties(paragraph_style, props);
        let mark_font = font_properties(
            base_font.clone(),
            props.and_then(|p| child(p, "rPr")),
            styles.theme,
        );
        let mut runs = Vec::new();
        let mut page_field = false;
        for run in node.descendants().filter(|n| is(*n, "r")) {
            let rprops = child(run, "rPr");
            if rprops.and_then(|p| child(p, "vanish")).is_some_and(enabled) {
                continue;
            }
            let mut font = base_font.clone();
            if let Some(id) = rprops.and_then(|p| val(p, "rStyle")) {
                font = styles.character(font, id)?;
            }
            font = font_properties(font, rprops, styles.theme);
            let mut text = String::new();
            for element in run.children() {
                if is(element, "t") {
                    text.push_str(element.text().unwrap_or(""));
                } else if is(element, "tab") {
                    text.push('\t');
                } else if is(element, "br") || is(element, "cr") {
                    if attr(element, "type") == Some("page") {
                        warnings.insert(
                            "Явный разрыв внутри абзаца требует отдельной поддержки".into(),
                        );
                    }
                    text.push('\n');
                } else if is(element, "instrText")
                    && element
                        .text()
                        .unwrap_or("")
                        .split_whitespace()
                        .any(|word| word == "PAGE")
                {
                    page_field = true;
                } else if is(element, "noBreakHyphen") {
                    text.push('\u{2011}');
                }
            }
            if !text.is_empty() {
                runs.push(Run { text, font });
            }
        }
        if runs.iter().map(|r| r.text.len()).sum::<usize>() > 200_000 {
            return Err("Абзац слишком большой для просмотра".into());
        }
        result.push(Paragraph {
            runs,
            style: paragraph_style,
            mark_font,
            style_id,
            list_label: label,
            page_field,
        });
    }
    Ok(result)
}

impl Document {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| format!("Не удалось открыть файл: {e}"))?
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        Self::from_bytes(bytes)
    }
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        let package = Package::parse(bytes)?;
        let main = relationship(&package, "_rels/.rels", "", "officeDocument")?
            .ok_or("Пакет не содержит документ Word")?;
        let content_types = package.text("[Content_Types].xml")?;
        let ct = parse_xml(&content_types).map_err(|e| e.to_string())?;
        if !ct.descendants().any(|n| {
            n.attribute("PartName") == Some(format!("/{main}").as_str())
                && n.attribute("ContentType")
                    .is_some_and(|v| v.ends_with("wordprocessingml.document.main+xml"))
        }) {
            return Err("Поддерживаются DOCX без макросов".into());
        }
        let (directory, name) = main.rsplit_once('/').unwrap_or(("", ""));
        let rel_path = format!("{directory}/_rels/{name}.rels");
        let style_part = relationship(&package, &rel_path, &main, "styles")?;
        let style_source = if let Some(path) = style_part {
            package.text(&path)?
        } else {
            format!(
                "<w:styles xmlns:w=\"{W}\"><w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"/></w:styles>"
            )
        };
        let styles_xml = parse_xml(&style_source).map_err(|e| format!("Стили: {e}"))?;
        let theme_part = relationship(&package, &rel_path, &main, "theme")?;
        let theme_source = theme_part
            .map(|p| package.text(&p))
            .transpose()?
            .unwrap_or_default();
        let theme_xml = if theme_source.is_empty() {
            None
        } else {
            Some(parse_xml(&theme_source).map_err(|e| format!("Тема: {e}"))?)
        };
        let theme = theme_xml
            .as_ref()
            .and_then(|xml| {
                xml.descendants()
                    .find(|n| n.tag_name().name() == "minorFont")
            })
            .and_then(|n| n.children().find(|n| n.tag_name().name() == "latin"))
            .and_then(|n| n.attribute("typeface"))
            .unwrap_or("Times New Roman");
        let default_id = styles_xml
            .descendants()
            .find(|n| {
                is(*n, "style")
                    && attr(*n, "type") == Some("paragraph")
                    && attr(*n, "default") == Some("1")
            })
            .and_then(|n| attr(n, "styleId"))
            .unwrap_or("")
            .to_owned();
        let styles = Styles {
            xml: &styles_xml,
            theme,
            default_id,
        };
        let document_source = package.text(&main)?;
        let xml = parse_xml(&document_source).map_err(|e| format!("Документ: {e}"))?;
        let numbering_source = relationship(&package, &rel_path, &main, "numbering")?
            .map(|p| package.text(&p))
            .transpose()?;
        let numbering = numbering_source
            .as_deref()
            .map(parse_xml)
            .transpose()
            .map_err(|e| e.to_string())?;
        let mut warnings = BTreeSet::new();
        for (tag, label) in [
            ("tbl", "таблицы"),
            ("drawing", "рисунки"),
            ("txbxContent", "текстовые блоки"),
            ("headerReference", "верхние колонтитулы"),
            ("footnoteReference", "сноски"),
            ("endnoteReference", "концевые сноски"),
        ] {
            if xml.descendants().any(|n| is(n, tag)) {
                return Err(format!(
                    "Этот просмотрщик пока не отображает {label}. Исходник не изменён."
                ));
            }
        }
        if xml
            .descendants()
            .any(|n| n.is_element() && n.tag_name().name() == "oMath")
        {
            return Err("Формулы пока не поддерживаются просмотрщиком".into());
        }
        let sections: Vec<_> = xml.descendants().filter(|n| is(*n, "sectPr")).collect();
        if sections.len() > 1 {
            return Err("Несколько разделов пока не поддерживаются просмотрщиком".into());
        }
        let mut page = Page::default();
        if let Some(section) = sections.first().copied() {
            if child(section, "cols")
                .and_then(|n| attr(n, "num"))
                .is_some_and(|v| v != "1")
            {
                return Err("Многоколоночная вёрстка пока не поддерживается".into());
            }
            if let Some(size) = child(section, "pgSz") {
                if let Some(v) = number(attr(size, "w")) {
                    page.width = v / 15.0;
                }
                if let Some(v) = number(attr(size, "h")) {
                    page.height = v / 15.0;
                }
            }
            if let Some(margin) = child(section, "pgMar") {
                for (name, field) in [
                    ("left", &mut page.left),
                    ("right", &mut page.right),
                    ("top", &mut page.top),
                    ("bottom", &mut page.bottom),
                    ("footer", &mut page.footer_distance),
                ] {
                    if let Some(v) = number(attr(margin, name)) {
                        *field = v / 15.0;
                    }
                }
            }
            page.different_first = child(section, "titlePg").is_some_and(enabled);
        }
        if !(100.0..=10000.0).contains(&page.width)
            || !(100.0..=10000.0).contains(&page.height)
            || page.width - page.left - page.right < 32.0
            || page.height - page.top - page.bottom < 32.0
            || [
                page.left,
                page.right,
                page.top,
                page.bottom,
                page.footer_distance,
            ]
            .iter()
            .any(|v| *v < 0.0)
        {
            return Err("Недопустимые размеры страницы или полей".into());
        }
        let paragraphs = read_paragraphs(&xml, &styles, numbering.as_ref(), &mut warnings)?;
        let footer_source = relationship(&package, &rel_path, &main, "footer")?
            .map(|p| package.text(&p))
            .transpose()?;
        let footer_xml = footer_source
            .as_deref()
            .map(parse_xml)
            .transpose()
            .map_err(|e| e.to_string())?;
        let footer = footer_xml
            .as_ref()
            .map(|xml| read_paragraphs(xml, &styles, numbering.as_ref(), &mut warnings))
            .transpose()?
            .unwrap_or_default();
        let settings_source = relationship(&package, &rel_path, &main, "settings")?
            .map(|p| package.text(&p))
            .transpose()?;
        let settings = settings_source
            .as_deref()
            .map(parse_xml)
            .transpose()
            .map_err(|e| e.to_string())?;
        let tab_stop = settings
            .as_ref()
            .and_then(|xml| xml.descendants().find(|n| is(*n, "defaultTabStop")))
            .and_then(|n| number(attr(n, "val")))
            .unwrap_or(720.0)
            / 15.0;
        let compress_justification = settings.as_ref().is_some_and(|xml| {
            xml.descendants().any(|n| {
                (is(n, "compatSetting")
                    && attr(n, "name") == Some("compatibilityMode")
                    && number(attr(n, "val")).is_some_and(|v| v >= 15.0))
                    || (is(n, "doWordPerfect6ParagraphJustification") && enabled(n))
            })
        });
        if paragraphs.len() > 10000 {
            return Err("В документе больше 10000 абзацев".into());
        }
        for paragraph in paragraphs.iter().chain(footer.iter()) {
            if std::iter::once(&paragraph.mark_font)
                .chain(paragraph.runs.iter().map(|r| &r.font))
                .any(|f| !(1.0..=400.0).contains(&f.points))
            {
                return Err(
                    "Размер шрифта вне поддерживаемого диапазона 1–400 пт; оформление не изменено"
                        .into(),
                );
            }
            let value = match paragraph.style.line {
                LineSpacing::Multiple(v) | LineSpacing::Exact(v) | LineSpacing::AtLeast(v) => v,
            };
            if value <= 0.0 {
                return Err("Неподдерживаемый нулевой или отрицательный интервал".into());
            }
        }
        let saved_page_count = if package.contains("docProps/app.xml") {
            let source = package.text("docProps/app.xml")?;
            parse_xml(&source)
                .ok()
                .and_then(|xml| {
                    xml.descendants()
                        .find(|n| n.is_element() && n.tag_name().name() == "Pages")
                        .and_then(|n| n.text())
                        .and_then(|v| v.parse::<usize>().ok())
                })
                .filter(|v| *v > 0 && *v <= 100000)
        } else {
            None
        };
        Ok(Self {
            compress_justification,
            package,
            paragraphs,
            footer,
            page,
            warnings: warnings.into_iter().collect(),
            tab_stop,
            saved_page_count,
        })
    }
}

#[cfg(test)]
mod tests;
