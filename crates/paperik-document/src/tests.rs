use super::*;
use std::io::Write;

fn fixture_parts() -> Vec<(String, String)> {
    vec![
        ("[Content_Types].xml".into(),r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#.into()),
        ("_rels/.rels".into(),r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="r1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.into()),
        ("word/_rels/document.xml.rels".into(),r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="r2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#.into()),
        ("word/styles.xml".into(),format!(r#"<w:styles xmlns:w="{W}"><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:pPr><w:spacing w:line="360"/><w:ind w:firstLine="709"/></w:pPr><w:rPr><w:rFonts w:ascii="Times New Roman"/><w:sz w:val="28"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Title"><w:basedOn w:val="Normal"/><w:rPr><w:sz w:val="32"/><w:b/></w:rPr></w:style></w:styles>"#)),
        ("word/document.xml".into(),format!(r#"<w:document xmlns:w="{W}"><w:body><w:p><w:pPr><w:jc w:val="right"/><w:rPr><w:sz w:val="32"/></w:rPr></w:pPr><w:r><w:t xml:space="preserve"> Имя </w:t></w:r><w:r><w:rPr><w:i/></w:rPr><w:t>автора</w:t></w:r></w:p><w:p><w:pPr><w:pStyle w:val="Title"/></w:pPr><w:r><w:t>Заголовок</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:left="1701" w:right="851" w:top="1134" w:bottom="1134"/></w:sectPr></w:body></w:document>"#)),
    ]
}

fn archive(parts: &[(String, String)], compressed: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut directory = Vec::new();
    for (name, text) in parts {
        let original = text.as_bytes();
        let encoded = if compressed {
            let mut encoder =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(original).unwrap();
            encoder.finish().unwrap()
        } else {
            original.to_vec()
        };
        let method = if compressed { 8_u16 } else { 0 };
        let offset = bytes.len() as u32;
        let crc = package::crc32(original);
        bytes.extend_from_slice(b"PK\x03\x04");
        bytes.extend(20_u16.to_le_bytes());
        bytes.extend(0_u16.to_le_bytes());
        bytes.extend(method.to_le_bytes());
        bytes.extend([0_u8; 4]);
        bytes.extend(crc.to_le_bytes());
        bytes.extend((encoded.len() as u32).to_le_bytes());
        bytes.extend((original.len() as u32).to_le_bytes());
        bytes.extend((name.len() as u16).to_le_bytes());
        bytes.extend(0_u16.to_le_bytes());
        bytes.extend(name.as_bytes());
        bytes.extend(encoded);
        directory.extend_from_slice(b"PK\x01\x02");
        directory.extend(20_u16.to_le_bytes());
        directory.extend(20_u16.to_le_bytes());
        directory.extend(0_u16.to_le_bytes());
        directory.extend(method.to_le_bytes());
        directory.extend([0_u8; 4]);
        directory.extend(crc.to_le_bytes());
        directory.extend(((bytes.len() - offset as usize - 30 - name.len()) as u32).to_le_bytes());
        directory.extend((original.len() as u32).to_le_bytes());
        directory.extend((name.len() as u16).to_le_bytes());
        directory.extend([0_u8; 12]);
        directory.extend(offset.to_le_bytes());
        directory.extend(name.as_bytes());
    }
    let offset = bytes.len() as u32;
    let size = directory.len() as u32;
    bytes.extend(directory);
    bytes.extend_from_slice(b"PK\x05\x06");
    bytes.extend([0_u8; 4]);
    bytes.extend((parts.len() as u16).to_le_bytes());
    bytes.extend((parts.len() as u16).to_le_bytes());
    bytes.extend(size.to_le_bytes());
    bytes.extend(offset.to_le_bytes());
    bytes.extend(0_u16.to_le_bytes());
    bytes
}

#[test]
fn crc_matches_the_standard_check_value() {
    assert_eq!(package::crc32(b"123456789"), 0xcbf4_3926)
}

#[test]
fn inherited_and_direct_properties_remain_distinct() {
    let bytes = archive(&fixture_parts(), false);
    let doc = Document::from_bytes(bytes.clone()).unwrap();
    assert_eq!(doc.paragraphs.len(), 2);
    let first = &doc.paragraphs[0];
    assert_eq!(first.text(), " Имя автора");
    assert_eq!(first.style.alignment, Alignment::Right);
    assert_eq!(first.runs[0].font.points, 14.0);
    assert_eq!(first.mark_font.points, 16.0);
    assert!(first.runs[1].font.italic);
    assert!(!first.runs[0].font.italic);
    assert_eq!(first.style.line, LineSpacing::Multiple(1.5));
    assert!((first.style.first_line - 709.0 / 15.0).abs() < 0.001);
    assert!(doc.paragraphs[1].runs[0].font.bold);
    assert_eq!(doc.paragraphs[1].runs[0].font.points, 16.0);
    assert!((doc.page.left - 113.4).abs() < 0.001);
    assert_eq!(&*doc.package.original, bytes);
}

#[test]
fn deflate_and_stored_parts_produce_the_same_document() {
    let parts = fixture_parts();
    let stored = Document::from_bytes(archive(&parts, false)).unwrap();
    let deflated = Document::from_bytes(archive(&parts, true)).unwrap();
    assert_eq!(stored.paragraphs, deflated.paragraphs);
}

#[test]
fn strict_wordprocessingml_namespace_is_supported() {
    let parts: Vec<_> = fixture_parts()
        .into_iter()
        .map(|(name, text)| (name, text.replace(W, WS)))
        .collect();
    assert_eq!(
        Document::from_bytes(archive(&parts, false))
            .unwrap()
            .paragraphs[0]
            .runs[0]
            .font
            .points,
        14.0
    );
}

#[test]
fn bad_crc_and_duplicate_parts_are_rejected() {
    let parts = fixture_parts();
    let mut bytes = archive(&parts, false);
    bytes[30 + parts[0].0.len() + 20] ^= 1;
    assert!(
        Document::from_bytes(bytes)
            .unwrap_err()
            .contains("контрольная")
    );
    let mut parts = fixture_parts();
    parts.push(parts[0].clone());
    assert!(Document::from_bytes(archive(&parts, false)).is_err());
}

#[test]
fn style_cycles_and_external_main_parts_are_rejected() {
    let mut parts = fixture_parts();
    parts[3].1 = parts[3].1.replace(
        "<w:rPr><w:rFonts",
        r#"<w:basedOn w:val="Title"/><w:rPr><w:rFonts"#,
    );
    assert!(
        Document::from_bytes(archive(&parts, false))
            .unwrap_err()
            .contains("Цикл")
    );
    let mut parts = fixture_parts();
    parts[1].1 = parts[1]
        .1
        .replace("Target=", r#"TargetMode="External" Target="#);
    assert!(Document::from_bytes(archive(&parts, false)).is_err());
}

#[test]
fn dangerous_paths_and_doctypes_are_rejected() {
    let mut parts = fixture_parts();
    parts.push(("../outside".into(), "data".into()));
    assert!(Document::from_bytes(archive(&parts, false)).is_err());
    let mut parts = fixture_parts();
    parts[4].1 = format!(
        "<!DOCTYPE document [<!ENTITY x SYSTEM \"file:///example\">]>{}",
        parts[4].1
    );
    assert!(Document::from_bytes(archive(&parts, false)).is_err());
}

#[test]
fn unsupported_objects_are_not_silently_dropped() {
    let mut parts = fixture_parts();
    parts[4].1 = parts[4].1.replace("<w:body>", "<w:body><w:tbl/>");
    assert!(
        Document::from_bytes(archive(&parts, false))
            .unwrap_err()
            .contains("таблицы")
    );
}

#[test]
fn typography_features_follow_style_inheritance_and_direct_overrides() {
    let mut parts = fixture_parts();
    parts[3].1=parts[3].1.replace("<w:sz w:val=\"28\"/>",r#"<w:sz w:val="28"/><w:kern w:val="24"/><w14:ligatures xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" w14:val="standardContextual"/>"#);
    parts[4].1=parts[4].1.replace("<w:i/>",r#"<w:i/><w:kern w:val="0"/><w14:ligatures xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" w14:val="none"/>"#);
    let doc = Document::from_bytes(archive(&parts, false)).unwrap();
    assert_eq!(doc.paragraphs[0].runs[0].font.kerning_from, Some(12.0));
    assert_eq!(doc.paragraphs[0].runs[0].font.ligatures, 3);
    assert_eq!(doc.paragraphs[0].runs[1].font.kerning_from, Some(0.0));
    assert_eq!(doc.paragraphs[0].runs[1].font.ligatures, 0);
}

#[test]
fn justification_compression_is_selected_by_document_compatibility() {
    let mut parts = fixture_parts();
    assert!(
        !Document::from_bytes(archive(&parts, false))
            .unwrap()
            .compress_justification
    );
    parts[2].1=parts[2].1.replace("</Relationships>",r#"<Relationship Id="settings" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/></Relationships>"#);
    parts.push(("word/settings.xml".into(),format!(r#"<w:settings xmlns:w="{W}"><w:compat><w:compatSetting w:name="compatibilityMode" w:val="15"/></w:compat></w:settings>"#)));
    assert!(
        Document::from_bytes(archive(&parts, false))
            .unwrap()
            .compress_justification
    );
}
