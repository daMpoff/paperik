use super::*;
use crate::{
    editor::{Edit, Editor, Position},
    tests::{archive, fixture_parts},
};
fn document(parts: &[(String, String)]) -> Document {
    Document::from_bytes(archive(parts, true)).unwrap()
}
fn apply(editor: &mut Editor, edit: Edit) {
    if let Some(change) = editor.prepare(edit).unwrap() {
        editor.commit(change).unwrap();
    }
}
fn replace(editor: &mut Editor, a: Position, b: Position, text: &str) {
    editor.set_position(a, false);
    editor.set_position(b, true);
    apply(editor, Edit::Insert(text.into()));
}
fn pos(paragraph: usize, offset: u32) -> Position {
    Position { paragraph, offset }
}
#[test]
fn unchanged_export_is_byte_identical() {
    let doc = document(&fixture_parts());
    assert_eq!(doc.to_docx(Some(20)).unwrap(), &*doc.package.original);
}
#[test]
fn equal_resolved_fonts_do_not_erase_distinct_source_properties() {
    let mut parts = fixture_parts();
    parts[4].1 = format!(
        r#"<w:document xmlns:w="{W}"><w:body><w:p><w:r><w:rPr><w:u w:val="double"/></w:rPr><w:t>A</w:t></w:r><w:r><w:rPr><w:u w:val="single"/></w:rPr><w:t>B</w:t></w:r></w:p></w:body></w:document>"#
    );
    let original = document(&parts);
    let mut editor = Editor::new(original.clone());
    replace(&mut editor, pos(0, 0), pos(0, 1), "");
    apply(&mut editor, Edit::Insert("A".into()));
    assert!(same_document(editor.document(), &original));
    let bytes = editor.document().to_docx(None).unwrap();
    assert_ne!(&bytes, &*original.package.original);
    let saved = Document::from_bytes(bytes).unwrap();
    let source = saved.package.text("word/document.xml").unwrap();
    assert!(source.contains(r#"w:val="single""#));
    assert!(!source.contains(r#"w:val="double""#));
}
#[test]
fn empty_paragraph_inherits_mark_font_and_locally_scoped_properties() {
    let mut parts = fixture_parts();
    parts[4].1 = format!(
        r#"<w:document xmlns:w="{W}"><w:body><w:p xmlns:x="urn:attributes" x:flag="one&#10;two&#9;three"><w:pPr xmlns:q="urn:properties"><w:rPr q:custom="kept"><w:sz w:val="32"/><w:lang w:val="ru-RU"/></w:rPr></w:pPr></w:p></w:body></w:document>"#
    );
    let mut editor = Editor::new(document(&parts));
    apply(&mut editor, Edit::Insert("Новый текст".into()));
    let saved = Document::from_bytes(editor.document().to_docx(None).unwrap()).unwrap();
    assert_eq!(saved.paragraphs[0].runs[0].font.points, 16.0);
    let source = saved.package.text("word/document.xml").unwrap();
    let xml = parse_xml(&source).unwrap();
    let paragraph = xml.descendants().find(|n| is(*n, "p")).unwrap();
    assert_eq!(
        paragraph.attribute(("urn:attributes", "flag")),
        Some("one\ntwo\tthree")
    );
    let run = child(paragraph, "r").unwrap();
    assert_eq!(
        child(run, "rPr")
            .unwrap()
            .attribute(("urn:properties", "custom")),
        Some("kept")
    );
}
#[test]
fn replacement_across_runs_preserves_properties_and_every_other_part() {
    let mut parts = fixture_parts();
    parts[4].1=parts[4].1.replace("<w:i/>",r#"<w:i/><w:lang w:val="ru-RU"/><w:shd w:fill="ABCDEF"/><x:custom xmlns:x="urn:kept" x:flag="yes"/>"#);
    parts.push((
        "word/media/opaque.bin".into(),
        "\0Opaque binary data\0".into(),
    ));
    parts.push(("docProps/app.xml".into(),r#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Pages>2</Pages><Unknown>untouched</Unknown></Properties>"#.into()));
    let original = document(&parts);
    let mut editor = Editor::new(original.clone());
    replace(&mut editor, pos(0, 2), pos(0, 7), " новый & <текст> ");
    let bytes = editor.document().to_docx(Some(3)).unwrap();
    let reopened = Document::from_bytes(bytes).unwrap();
    assert!(same_document(editor.document(), &reopened));
    for name in original
        .package
        .names()
        .filter(|name| !matches!(*name, "word/document.xml" | "docProps/app.xml"))
    {
        assert_eq!(
            original.package.bytes(name).unwrap(),
            reopened.package.bytes(name).unwrap(),
            "{name}"
        );
    }
    let xml = reopened.package.text("word/document.xml").unwrap();
    assert!(xml.contains(r#"<w:lang w:val="ru-RU"/>"#));
    assert!(xml.contains(r#"<x:custom xmlns:x="urn:kept" x:flag="yes"/>"#));
    assert!(xml.contains("&amp;") && xml.contains("&lt;текст&gt;"));
    assert_eq!(reopened.saved_page_count, Some(3));
    assert!(
        reopened
            .package
            .text("docProps/app.xml")
            .unwrap()
            .contains("<Unknown>untouched</Unknown>")
    );
}
#[test]
fn new_paragraphs_soft_breaks_tabs_and_unicode_round_trip() {
    let mut editor = Editor::new(document(&fixture_parts()));
    editor.set_position(pos(0, 0), false);
    apply(
        &mut editor,
        Edit::Insert("Привет 🚀\nвторая\tстрока".into()),
    );
    apply(&mut editor, Edit::SoftBreak);
    apply(&mut editor, Edit::Insert("е\u{301} конец".into()));
    let saved = editor.document().to_docx(None).unwrap();
    let reopened = Document::from_bytes(saved).unwrap();
    assert_eq!(reopened.paragraphs[0].text(), "Привет 🚀");
    assert!(
        reopened.paragraphs[1]
            .text()
            .starts_with("вторая\tстрока\nе\u{301} конец")
    );
    assert!(same_document(editor.document(), &reopened));
}
#[test]
fn cloned_paragraph_keeps_unknown_properties_without_duplicate_ids_or_page_breaks() {
    let mut parts = fixture_parts();
    parts[4].1=parts[4].1.replacen("<w:p>",r#"<w:p xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" w14:paraId="12345678" w14:textId="87654321">"#,1)
        .replacen("<w:jc",r#"<w:pageBreakBefore/><w:customProperty w:val="keep"/><w:jc"#,1);
    let mut editor = Editor::new(document(&parts));
    editor.set_position(pos(0, 0), false);
    apply(&mut editor, Edit::Insert("new\n".into()));
    let saved = Document::from_bytes(editor.document().to_docx(None).unwrap()).unwrap();
    let xml = saved.package.text("word/document.xml").unwrap();
    assert_eq!(xml.matches("12345678").count(), 1);
    assert_eq!(
        xml.matches(r#"<w:customProperty w:val="keep"/>"#).count(),
        2
    );
    assert!(saved.paragraphs[0].style.page_break);
    assert!(!saved.paragraphs[1].style.page_break);
}
#[test]
fn protected_objects_are_preserved_and_editing_or_deleting_them_is_refused() {
    let mut parts = fixture_parts();
    parts[4].1 = parts[4].1.replacen(
        "<w:r>",
        r#"<w:bookmarkStart w:id="1" w:name="kept"/><w:bookmarkEnd w:id="1"/><w:r>"#,
        1,
    );
    let mut editor = Editor::new(document(&parts));
    assert!(editor.prepare(Edit::Insert("x".into())).is_err());
    editor.set_position(pos(1, 0), false);
    apply(&mut editor, Edit::Insert("new".into()));
    let saved = Document::from_bytes(editor.document().to_docx(None).unwrap()).unwrap();
    let original = document(&parts);
    let before = original.package.text("word/document.xml").unwrap();
    let after = saved.package.text("word/document.xml").unwrap();
    let a = parse_xml(&before).unwrap();
    let b = parse_xml(&after).unwrap();
    assert_eq!(
        raw(&before, a.descendants().find(|n| is(*n, "p")).unwrap()),
        raw(&after, b.descendants().find(|n| is(*n, "p")).unwrap())
    );
    let mut invalid = editor.document().clone();
    invalid.paragraphs.remove(0);
    assert!(invalid.to_docx(None).is_err());
}
#[test]
fn strict_namespace_and_local_prefixes_are_retained() {
    let mut parts = fixture_parts();
    for (_, text) in &mut parts {
        *text = text
            .replace(W, WS)
            .replace("w:", "a:")
            .replace("xmlns:w", "xmlns:a");
    }
    let mut editor = Editor::new(document(&parts));
    apply(&mut editor, Edit::Insert("local & namespace".into()));
    let saved = Document::from_bytes(editor.document().to_docx(None).unwrap()).unwrap();
    assert!(saved.paragraphs[0].text().starts_with("local & namespace"));
    assert!(
        saved
            .package
            .text("word/document.xml")
            .unwrap()
            .contains(WS)
    );
}
#[test]
fn cached_proofing_and_page_break_markers_are_invalidated_only_in_edited_paragraphs() {
    let mut parts = fixture_parts();
    parts[4].1=parts[4].1.replacen("<w:r>",r#"<w:proofErr w:type="spellStart"/><w:proofErr w:type="spellEnd"/><w:r><w:lastRenderedPageBreak/>"#,1);
    let mut editor = Editor::new(document(&parts));
    apply(&mut editor, Edit::Insert("x".into()));
    let saved = Document::from_bytes(editor.document().to_docx(None).unwrap()).unwrap();
    let xml = saved.package.text("word/document.xml").unwrap();
    assert!(!xml.contains("proofErr") && !xml.contains("lastRenderedPageBreak"));
}
#[test]
fn saved_revision_survives_undo_and_redo_without_clearing_history() {
    let mut editor = Editor::new(document(&fixture_parts()));
    apply(&mut editor, Edit::Insert("a".into()));
    editor.mark_saved();
    assert!(!editor.dirty());
    apply(&mut editor, Edit::Insert("b".into()));
    assert!(editor.dirty());
    apply(&mut editor, Edit::Undo);
    assert!(!editor.dirty());
    apply(&mut editor, Edit::Redo);
    assert!(editor.dirty());
    apply(&mut editor, Edit::Undo);
    apply(&mut editor, Edit::Undo);
    assert!(editor.dirty());
}
#[test]
fn digital_signatures_and_cross_style_merges_are_not_silently_damaged() {
    let mut parts = fixture_parts();
    parts.push(("_xmlsignatures/sig1.xml".into(), "<signature/>".into()));
    let mut editor = Editor::new(document(&parts));
    assert!(editor.document().to_docx(None).is_ok());
    apply(&mut editor, Edit::Insert("new".into()));
    assert!(
        editor
            .document()
            .to_docx(None)
            .unwrap_err()
            .contains("подписью")
    );
    let mut editor = Editor::new(document(&fixture_parts()));
    let offset = crate::editor::utf16_len(&editor.document().paragraphs[0].text());
    editor.set_position(pos(0, offset), false);
    apply(&mut editor, Edit::Delete { word: false });
    assert!(
        editor
            .document()
            .to_docx(None)
            .unwrap_err()
            .contains("разными стилями")
    );
}
#[test]
fn new_file_save_never_overwrites_and_cleans_up_after_failures() {
    let root = std::env::temp_dir().join(format!(
        "paperik-save-{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    let target = root.join("Копия отчёта.docx");
    let mut editor = Editor::new(document(&fixture_parts()));
    apply(&mut editor, Edit::Insert("new".into()));
    editor.document().save_new(&target, Some(1)).unwrap();
    let before = fs::read(&target).unwrap();
    assert!(same_document(
        editor.document(),
        &Document::from_bytes(before.clone()).unwrap()
    ));
    assert!(editor.document().save_new(&target, None).is_err());
    assert_eq!(fs::read(&target).unwrap(), before);
    assert!(
        editor
            .document()
            .save_new(&root.join("missing/file.docx"), None)
            .is_err()
    );
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    fs::remove_file(target).unwrap();
    fs::remove_dir(root).unwrap();
}
