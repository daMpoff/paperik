use super::*;
use crate::{
    Block,
    editor::{Edit, Editor, Position, utf16_len},
};
const FIXTURE: &[u8] = include_bytes!("../../../../../tests/fixtures/blocks.docx");
fn editor() -> Editor {
    Editor::new(Document::from_bytes(FIXTURE.to_vec()).unwrap())
}
fn apply(e: &mut Editor, edit: Edit) {
    if let Some(change) = e.prepare(edit).unwrap() {
        e.commit(change).unwrap();
    }
}
fn point(e: &mut Editor, p: usize, n: u32) {
    e.set_position(
        Position {
            paragraph: p,
            offset: n,
        },
        false,
    );
}
fn cell(e: &Editor, row: usize, column: usize) -> Vec<usize> {
    e.document()
        .content
        .as_ref()
        .unwrap()
        .blocks
        .iter()
        .find_map(|b| {
            if let Block::Table(t) = b {
                Some(t.rows[row].cells[column].paragraphs.clone())
            } else {
                None
            }
        })
        .unwrap()
}
fn tags(doc: &Document, tag: &str) -> Vec<String> {
    let source = doc.package.text("word/document.xml").unwrap();
    let xml = crate::parse_xml(&source).unwrap();
    xml.descendants()
        .filter(|n| is(*n, tag))
        .map(|n| raw(&source, n).into())
        .collect()
}
fn verify(e: &Editor) -> Document {
    let bytes = e.document().to_docx(None).unwrap();
    let reopened = Document::from_bytes(bytes).unwrap();
    assert!(same_document(e.document(), &reopened));
    let original = Document::from_bytes(FIXTURE.to_vec()).unwrap();
    for tag in ["tblPr", "tblGrid", "tcPr", "drawing"] {
        assert_eq!(tags(&original, tag), tags(&reopened, tag), "{tag}");
    }
    for name in original
        .package
        .names()
        .filter(|n| *n != "word/document.xml")
    {
        assert_eq!(
            original.package.bytes(name).unwrap(),
            reopened.package.bytes(name).unwrap(),
            "{name}"
        );
    }
    reopened
}
#[test]
fn text_in_cells_saves_without_touching_neighbours_or_objects() {
    let mut e = editor();
    let index = cell(&e, 1, 0)[0];
    let before = tags(e.document(), "tc");
    point(&mut e, index, 0);
    apply(&mut e, Edit::Insert("Новый текст 🚀 ".into()));
    let reopened = verify(&e);
    let after = tags(&reopened, "tc");
    assert!(
        reopened.paragraphs[index]
            .text()
            .starts_with("Новый текст 🚀 ")
    );
    for (i, (a, b)) in before.iter().zip(&after).enumerate() {
        if i != 2 {
            assert_eq!(a, b, "unchanged cell {i}");
        }
    }
    assert_eq!(
        e.document().paragraphs[index].runs[0].font,
        reopened.paragraphs[index].runs[0].font
    );
}
#[test]
fn splitting_and_merging_inside_cell_rebinds_all_following_blocks() {
    let mut e = editor();
    let original = e.document().clone();
    let index = cell(&e, 1, 0)[0];
    point(&mut e, index, 6);
    apply(&mut e, Edit::Insert("\nДополнение ".into()));
    let indices = cell(&e, 1, 0);
    assert_eq!(indices, vec![index, index + 1]);
    assert_eq!(cell(&e, 1, 1)[0], index + 2);
    assert_eq!(verify(&e).paragraphs.len(), 107);
    let (mut restored, _) =
        Editor::from_checkpoint(&e.checkpoint("Synthetic.docx").unwrap()).unwrap();
    assert_eq!(restored.document().content, e.document().content);
    verify(&restored);
    apply(&mut restored, Edit::Undo);
    assert_eq!(restored.document().paragraphs, original.paragraphs);
    assert_eq!(restored.document().content, original.content);
    assert_eq!(restored.document().to_docx(None).unwrap(), FIXTURE);
    apply(&mut restored, Edit::Redo);
    verify(&restored);
    point(&mut e, index + 1, 0);
    apply(&mut e, Edit::Backspace { word: false });
    assert_eq!(cell(&e, 1, 0), vec![index]);
    verify(&e);
}
#[test]
fn new_body_paragraphs_preserve_the_image_anchor_and_undo_redo() {
    let mut e = editor();
    let original = e.document().clone();
    point(&mut e, 0, 2);
    apply(&mut e, Edit::Insert("\nНовый абзац ".into()));
    let Block::Picture(p) = &e.document().content.as_ref().unwrap().blocks[2] else {
        panic!("missing picture")
    };
    assert_eq!(p.paragraph, 2);
    verify(&e);
    let (mut restored, _) =
        Editor::from_checkpoint(&e.checkpoint("Synthetic.docx").unwrap()).unwrap();
    assert_eq!(restored.document().content, e.document().content);
    verify(&restored);
    apply(&mut restored, Edit::Undo);
    assert_eq!(restored.document().paragraphs, original.paragraphs);
    assert_eq!(restored.document().content, original.content);
    apply(&mut restored, Edit::Redo);
    verify(&restored);
}
#[test]
fn boundary_replacements_deletions_and_image_edits_are_atomic_errors() {
    let mut e = editor();
    let original = e.document().clone();
    let a = cell(&e, 1, 0)[0];
    let b = cell(&e, 1, 1)[0];
    point(&mut e, a, 0);
    e.set_position(
        Position {
            paragraph: b,
            offset: 2,
        },
        true,
    );
    assert!(e.prepare(Edit::Insert("bad".into())).is_err());
    point(&mut e, b, 0);
    assert!(e.prepare(Edit::Backspace { word: false }).is_err());
    let len = utf16_len(&e.document().paragraphs[a].text());
    point(&mut e, a, len);
    assert!(e.prepare(Edit::Delete { word: false }).is_err());
    point(&mut e, 1, 0);
    assert!(e.prepare(Edit::Insert("image".into())).is_err());
    e.select_all();
    assert!(e.prepare(Edit::Insert("replace all".into())).is_err());
    assert_eq!(e.document().paragraphs, original.paragraphs);
    assert_eq!(e.document().content, original.content);
    assert!(!e.dirty());
}
#[test]
fn export_rejects_forged_geometry_anchors_and_cross_cell_font_sources() {
    let mut doc = editor().document().clone();
    let Block::Picture(p) = &mut doc.content.as_mut().unwrap().blocks[1] else {
        unreachable!()
    };
    p.width += 1.0;
    assert!(doc.to_docx(None).is_err());
    let mut doc = editor().document().clone();
    doc.paragraphs[4].source = Some(5);
    assert!(doc.to_docx(None).is_err());
    let mut doc = editor().document().clone();
    doc.paragraphs[4].runs[0].source = doc.paragraphs[5].runs[0].source;
    assert!(doc.to_docx(None).is_err());
    let mut doc = editor().document().clone();
    doc.paragraphs[1].runs.push(crate::Run {
        text: "bad".into(),
        picture: None,
        font: Default::default(),
        source: None,
    });
    assert!(doc.to_docx(None).is_err());
}
#[test]
fn empty_cell_typing_soft_breaks_and_unicode_round_trip() {
    let mut e = editor();
    let index = cell(&e, 1, 0)[0];
    let len = utf16_len(&e.document().paragraphs[index].text());
    point(&mut e, index, 0);
    e.set_position(
        Position {
            paragraph: index,
            offset: len,
        },
        true,
    );
    apply(&mut e, Edit::Insert(String::new()));
    assert!(e.document().paragraphs[index].text().is_empty());
    verify(&e);
    apply(&mut e, Edit::Insert("е\u{301} 🚀".into()));
    apply(&mut e, Edit::SoftBreak);
    apply(&mut e, Edit::Insert("Продолжение".into()));
    assert_eq!(cell(&e, 1, 0), vec![index]);
    assert!(verify(&e).paragraphs[index].text().contains('\n'));
}

#[test]
fn nested_runs_keep_direct_and_unmodelled_formatting_after_replacement() {
    let doc = Document::from_bytes(FIXTURE.to_vec()).unwrap();
    let source=doc.package.text("word/document.xml").unwrap().replace("<w:t>Строка 1</w:t>","<w:t>Строка </w:t></w:r><w:r><w:rPr><w:b/><w:color w:val=\"28664F\"/><w:lang w:val=\"ru-RU\"/></w:rPr><w:t>1234</w:t>");
    let bytes = doc
        .package
        .replace(&BTreeMap::from([(
            "word/document.xml".into(),
            source.into_bytes(),
        )]))
        .unwrap();
    let mut e = Editor::new(Document::from_bytes(bytes).unwrap());
    let i = cell(&e, 1, 0)[0];
    point(&mut e, i, 5);
    e.set_position(
        Position {
            paragraph: i,
            offset: 9,
        },
        true,
    );
    apply(&mut e, Edit::Insert("⚙".into()));
    let reopened = verify(&e);
    assert!(
        reopened.paragraphs[i]
            .runs
            .iter()
            .any(|r| r.font.bold && r.text == "34")
    );
    assert!(
        reopened
            .package
            .text("word/document.xml")
            .unwrap()
            .contains("<w:lang w:val=\"ru-RU\"/>")
    );
}
