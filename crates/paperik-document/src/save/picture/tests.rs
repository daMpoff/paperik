use super::*;
use crate::{
    Block, Picture, TableAction, TableEdit,
    editor::{Edit, Editor},
};

const FIXTURE: &[u8] = include_bytes!("../../../../../tests/fixtures/blocks.docx");

fn small_png() -> Arc<Vec<u8>> {
    // Synthetic 1 × 1 RGB pixel.
    Arc::new(vec![
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2,
        0, 0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 248, 151, 226, 6, 0,
        4, 12, 1, 169, 251, 67, 1, 146, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ])
}

fn pictures(doc: &Document) -> Vec<&Picture> {
    doc.content
        .iter()
        .flat_map(|c| &c.blocks)
        .filter_map(|block| {
            if let Block::Picture(picture) = block {
                Some(picture)
            } else {
                None
            }
        })
        .collect()
}

fn apply(editor: &mut Editor, edit: Edit) {
    let pending = editor.prepare(edit).unwrap().unwrap();
    editor.commit(pending).unwrap();
}

fn change(editor: &mut Editor, paragraph: usize, action: PictureAction) {
    apply(
        editor,
        Edit::Picture(PictureEdit {
            paragraph,
            offset: None,
            action,
        }),
    );
}

#[test]
fn replace_preserves_anchor_and_does_not_change_other_use_of_shared_picture() {
    let package = crate::Package::parse(FIXTURE.to_vec()).unwrap();
    let main = "word/document.xml";
    let mut source = package.text(main).unwrap();
    let xml = parse_xml(&source).unwrap();
    let p = xml
        .descendants()
        .find(|n| is(*n, "p") && n.descendants().any(|n| is(n, "drawing")))
        .unwrap();
    let end = p.range().end;
    let duplicate = raw(&source, p).replace("id=\"1\"", "id=\"2\"");
    source.insert_str(end, &duplicate);
    let original = package
        .replace(&BTreeMap::from([(main.into(), source.into_bytes())]))
        .unwrap();
    let mut editor = Editor::new(Document::from_bytes(original.clone()).unwrap());
    let before = pictures(editor.document());
    assert_eq!(before.len(), 2);
    let first = before[0].clone();
    let second = before[1].clone();
    assert!(Arc::ptr_eq(&first.bytes, &second.bytes));

    let raster = small_png();
    change(
        &mut editor,
        first.paragraph,
        PictureAction::Replace {
            bytes: raster.clone(),
        },
    );
    let copy = Document::from_bytes(editor.document().to_docx(None).unwrap()).unwrap();
    let after = pictures(&copy);
    assert_eq!(after[0].bytes, raster);
    assert_eq!(
        [after[0].width, after[0].height],
        [first.width, first.height]
    );
    assert_eq!(after[1], &second);

    let baseline = Document::from_bytes(original).unwrap();
    let before = baseline.package.text(main).unwrap();
    let after = copy.package.text(main).unwrap();
    assert_eq!(before, after.replacen("rIdPaperikImage1", "image", 1));
    for part in baseline
        .package
        .names()
        .filter(|name| ![main, "word/_rels/document.xml.rels", TYPES].contains(name))
    {
        assert_eq!(
            baseline.package.bytes(part).unwrap(),
            copy.package.bytes(part).unwrap(),
            "{part}"
        );
    }
}

#[test]
fn picture_commands_and_table_history_survive_recovery_and_export() {
    let mut e = Editor::new(Document::from_bytes(FIXTURE.to_vec()).unwrap());
    apply(&mut e, Edit::Insert("Набранный текст ".into()));
    change(
        &mut e,
        0,
        PictureAction::Insert {
            bytes: small_png(),
            size: [800, 800],
        },
    );
    let inserted = e.selection().focus.paragraph;
    assert!(
        e.document().paragraphs[0]
            .text()
            .starts_with("Набранный текст ")
    );
    assert_eq!(pictures(e.document()).len(), 2);
    let picture = pictures(e.document())[0];
    assert_eq!([picture.width, picture.height], [800.0 / 15.0; 2]);
    change(&mut e, inserted, PictureAction::Resize { size: [600, 600] });
    let table_anchor = e
        .document()
        .content
        .as_ref()
        .unwrap()
        .blocks
        .iter()
        .find_map(|block| {
            if let Block::Table(table) = block {
                Some(table.rows[1].cells[0].paragraphs[0])
            } else {
                None
            }
        })
        .unwrap();
    apply(
        &mut e,
        Edit::Table(TableEdit {
            paragraph: table_anchor,
            action: TableAction::InsertRow { after: true },
        }),
    );
    let saved = e.document().to_docx(None).unwrap();
    let reloaded = Document::from_bytes(saved.clone()).unwrap();
    let source = reloaded.package.text("word/document.xml").unwrap();
    assert!(source.contains("<wp:extent cx=\"381000\" cy=\"381000\"/>"));
    assert!(source.contains("<a:ext cx=\"381000\" cy=\"381000\"/>"));

    change(&mut e, inserted, PictureAction::Delete);
    assert_eq!(pictures(e.document()).len(), 1);
    assert!(
        !e.document()
            .package
            .contains("word/media/paperik-image-1.png")
    );
    assert!(
        !e.document()
            .package
            .text(TYPES)
            .unwrap()
            .contains("paperik-image-1.png")
    );
    assert!(
        !e.document()
            .package
            .text("word/_rels/document.xml.rels")
            .unwrap()
            .contains("rIdPaperikImage1")
    );
    let deleted = e.document().to_docx(None).unwrap();
    assert!(
        Document::from_bytes(deleted.clone()).unwrap().paragraphs[inserted]
            .text()
            .is_empty()
    );

    let checkpoint = e.checkpoint("Fixture.docx").unwrap();
    assert_eq!(checkpoint[15], 3);
    let (mut e, _) = Editor::from_checkpoint(&checkpoint).unwrap();
    apply(&mut e, Edit::Undo);
    assert_eq!(e.document().to_docx(None).unwrap(), saved);
    apply(&mut e, Edit::Redo);
    assert_eq!(e.document().to_docx(None).unwrap(), deleted);
    for _ in 0..5 {
        apply(&mut e, Edit::Undo);
    }
    assert_eq!(e.document().to_docx(None).unwrap(), FIXTURE);
}

#[test]
fn insert_picture_creates_missing_relationship_part_and_rejects_invalid_requests() {
    let mut parts = crate::tests::fixture_parts();
    parts.retain(|(name, _)| name != "word/_rels/document.xml.rels");
    let (_, main) = parts
        .iter_mut()
        .find(|(name, _)| name == "word/document.xml")
        .unwrap();
    *main = main.replace(r#"<w:pStyle w:val="Title"/>"#, "");
    let bytes = crate::tests::archive(&parts, true);
    let mut e = Editor::new(Document::from_bytes(bytes.clone()).unwrap());
    assert!(
        !e.document()
            .package
            .contains("word/_rels/document.xml.rels")
    );
    assert!(
        e.prepare(Edit::Picture(PictureEdit {
            paragraph: 0,
            offset: None,
            action: PictureAction::Delete
        }))
        .is_err()
    );
    assert!(
        e.prepare(Edit::Picture(PictureEdit {
            paragraph: 0,
            offset: None,
            action: PictureAction::Insert {
                bytes: Arc::new(vec![0]),
                size: [100, 100]
            }
        }))
        .is_err()
    );
    assert_eq!(e.document().to_docx(None).unwrap(), bytes);
    change(
        &mut e,
        0,
        PictureAction::Insert {
            bytes: small_png(),
            size: [300, 300],
        },
    );
    let copy = Document::from_bytes(e.document().to_docx(None).unwrap()).unwrap();
    assert_eq!(pictures(&copy).len(), 1);
    assert!(copy.package.contains("word/_rels/document.xml.rels"));
}

#[test]
fn inline_pictures_edit_independently_in_text_and_cells_and_recover() {
    use crate::editor::Position;

    let mut editor = Editor::new(Document::from_bytes(FIXTURE.to_vec()).unwrap());
    let original = editor.document().clone();
    let Block::Table(table) = &original.content.as_ref().unwrap().blocks[2] else {
        unreachable!()
    };
    let cell = table.rows[1].cells[0].paragraphs[0];
    let text_before = original.paragraphs[cell].text();
    let apply_inline = |editor: &mut Editor, paragraph, offset, action| {
        apply(
            editor,
            Edit::Picture(PictureEdit {
                paragraph,
                offset: Some(offset),
                action,
            }),
        );
    };
    let insert = || PictureAction::Insert {
        bytes: small_png(),
        size: [360, 240],
    };
    apply_inline(&mut editor, 0, 2, insert());
    apply_inline(&mut editor, cell, 1, insert());
    apply_inline(&mut editor, cell, 2, insert());
    apply_inline(
        &mut editor,
        cell,
        2,
        PictureAction::Resize { size: [720, 480] },
    );
    editor.set_position(
        Position {
            paragraph: cell,
            offset: 3,
        },
        false,
    );
    apply(&mut editor, Edit::Insert(" + ".into()));

    let saved = editor.document().to_docx(None).unwrap();
    let output = Document::from_bytes(saved).unwrap();
    assert_eq!(
        output.paragraphs[0]
            .text()
            .replace(crate::INLINE_OBJECT, ""),
        original.paragraphs[0].text()
    );
    let pics: Vec<_> = output.paragraphs[cell]
        .inline_pictures()
        .map(|(p, _, image)| (p, image.width))
        .collect();
    assert_eq!(pics, [(1, 24.0), (2, 48.0)]);
    assert_eq!(
        output.paragraphs[cell].style,
        original.paragraphs[cell].style
    );
    assert_eq!(output.content, original.content);
    let checkpoint = editor.checkpoint("Fixture.docx").unwrap();
    let (mut restored, _) = Editor::from_checkpoint(&checkpoint).unwrap();
    apply_inline(&mut restored, cell, 1, PictureAction::Delete);
    assert_eq!(
        restored.document().paragraphs[cell]
            .inline_pictures()
            .count(),
        1
    );
    apply_inline(&mut restored, cell, 1, PictureAction::Delete);
    assert_eq!(
        restored.document().paragraphs[cell]
            .text()
            .replace(" + ", ""),
        text_before
    );
    apply(&mut restored, Edit::Undo);
    assert_eq!(
        restored.document().paragraphs[cell]
            .inline_pictures()
            .count(),
        1
    );
}

#[test]
fn picture_only_paragraph_stays_in_the_text_flow() {
    let mut parts = crate::tests::fixture_parts();
    let (_, xml) = parts
        .iter_mut()
        .find(|(p, _)| p == "word/document.xml")
        .unwrap();
    *xml = xml.replace("<w:body>", "<w:body><w:p/>");
    let mut editor =
        Editor::new(Document::from_bytes(crate::tests::archive(&parts, false)).unwrap());
    apply(
        &mut editor,
        Edit::Picture(PictureEdit {
            paragraph: 0,
            offset: Some(0),
            action: PictureAction::Insert {
                bytes: small_png(),
                size: [250, 250],
            },
        }),
    );
    assert_eq!(editor.selection().focus.offset, 1);
    assert!(editor.document().content.is_none());
    apply(&mut editor, Edit::Insert(" Подпись в той же строке".into()));
    let doc = Document::from_bytes(editor.document().to_docx(None).unwrap()).unwrap();
    assert!(doc.paragraphs[0].text().starts_with("\u{fffc} Подпись"));
    assert!(doc.content.is_none());
}
