use super::*;
use crate::editor::{Edit, Editor, Position};

const FIXTURE: &[u8] = include_bytes!("../../../../../tests/fixtures/blocks.docx");

fn editor() -> Editor {
    Editor::new(Document::from_bytes(FIXTURE.to_vec()).unwrap())
}

fn table(document: &Document) -> &Table {
    document
        .content
        .as_ref()
        .unwrap()
        .blocks
        .iter()
        .find_map(|b| match b {
            Block::Table(table) => Some(table),
            _ => None,
        })
        .unwrap()
}

fn paragraph(editor: &Editor, row: usize, cell: usize) -> usize {
    table(editor.document()).rows[row].cells[cell].paragraphs[0]
}

fn apply(editor: &mut Editor, edit: Edit) {
    let change = editor.prepare(edit).unwrap().unwrap();
    editor.commit(change).unwrap();
}

fn command(editor: &mut Editor, row: usize, cell: usize, action: TableAction) {
    let paragraph = paragraph(editor, row, cell);
    apply(editor, Edit::Table(TableEdit { paragraph, action }));
}

fn roundtrip(editor: &Editor) -> Document {
    let before = Document::from_bytes(FIXTURE.to_vec()).unwrap();
    let output = editor.document().to_docx(None).unwrap();
    let after = Document::from_bytes(output).unwrap();
    assert!(same_document(editor.document(), &after));
    assert_eq!(&*editor.document().package.original, FIXTURE);
    for name in before.package.names().filter(|n| *n != "word/document.xml") {
        assert_eq!(
            before.package.bytes(name).unwrap(),
            after.package.bytes(name).unwrap(),
            "{name}"
        );
    }
    let outside_table = |doc: &Document| {
        let mut source = doc.package.text("word/document.xml").unwrap();
        let xml = parse_xml(&source).unwrap();
        let range = xml.descendants().find(|n| is(*n, "tbl")).unwrap().range();
        source.replace_range(range, "");
        source
    };
    assert_eq!(outside_table(&before), outside_table(&after));
    after
}

#[test]
fn rows_columns_and_dimensions_roundtrip_with_template_styles() {
    let mut e = editor();
    let initial = table(e.document()).clone();
    let rows = initial.rows.len();
    command(&mut e, 0, 0, TableAction::InsertRow { after: true });
    assert_eq!(table(e.document()).rows.len(), rows + 1);
    assert!(!table(e.document()).rows[1].header);
    let blank = paragraph(&e, 1, 0);
    let template = &e.document().paragraphs[paragraph(&e, 0, 0)];
    assert!(e.document().paragraphs[blank].text().is_empty());
    assert_eq!(e.document().paragraphs[blank].style, template.style);
    assert_eq!(
        table(e.document()).rows[1].cells[0].fill,
        initial.rows[0].cells[0].fill
    );
    apply(&mut e, Edit::Insert("Новая строка".into()));
    command(&mut e, 1, 0, TableAction::InsertColumn { after: true });
    assert_eq!(table(e.document()).columns.len(), 3);
    assert_eq!(table(e.document()).rows.last().unwrap().cells[0].span, 3);
    let total = table(e.document()).columns.iter().sum::<f32>();
    command(&mut e, 1, 0, TableAction::Width { twips: 1900 });
    assert!((table(e.document()).columns[0] - 1900.0 / 15.0).abs() < 0.01);
    assert!((table(e.document()).columns.iter().sum::<f32>() - total).abs() < 0.01);
    command(&mut e, 1, 0, TableAction::Height { twips: 600 });
    assert_eq!(table(e.document()).rows[1].min_height, 40.0);
    roundtrip(&e);
    command(&mut e, 1, 0, TableAction::AutoHeight);
    assert_eq!(table(e.document()).rows[1].min_height, 0.0);
    command(&mut e, 1, 1, TableAction::DeleteColumn);
    command(&mut e, 1, 0, TableAction::DeleteRow);
    let saved = roundtrip(&e);
    assert_eq!(table(&saved).rows.len(), rows);
    assert_eq!(table(&saved).columns.len(), 2);
}

#[test]
fn rectangle_merge_preserves_all_text_and_split_recreates_grid_cells() {
    let mut e = editor();
    let paragraphs: Vec<_> = (1..3)
        .flat_map(|r| {
            let e = &e;
            (0..2).map(move |c| e.document().paragraphs[paragraph(e, r, c)].clone())
        })
        .collect();
    let end_paragraph = paragraph(&e, 2, 1);
    command(&mut e, 1, 0, TableAction::Merge { end_paragraph });
    let t = table(e.document());
    assert_eq!(t.rows[1].cells[0].span, 2);
    assert_eq!(t.rows[1].cells[0].merge, Some(VerticalMerge::Start));
    assert_eq!(t.rows[2].cells[0].merge, Some(VerticalMerge::Continue));
    for (i, old) in t.rows[1].cells[0].paragraphs.iter().zip(&paragraphs) {
        assert!(same_paragraph(&e.document().paragraphs[*i], old));
    }
    assert_eq!(t.rows[1].cells[0].paragraphs.len(), 4);
    let hidden = t.rows[2].cells[0].paragraphs[0];
    e.set_position(
        Position {
            paragraph: hidden,
            offset: 0,
        },
        false,
    );
    assert!(e.prepare(Edit::Insert("скрыто".into())).is_err());
    roundtrip(&e);
    command(&mut e, 1, 0, TableAction::InsertRow { after: true });
    assert_eq!(
        table(e.document()).rows[2].cells[0].merge,
        Some(VerticalMerge::Continue)
    );
    command(&mut e, 1, 0, TableAction::Split);
    let saved = roundtrip(&e);
    for row in &table(&saved).rows[1..4] {
        assert_eq!(row.cells.len(), 2);
        assert!(row.cells.iter().all(|c| c.span == 1 && c.merge.is_none()));
    }
    assert_eq!(table(&saved).rows[1].cells[0].paragraphs.len(), 4);
}

#[test]
fn old_drafts_and_new_table_history_keep_pending_text_and_original_bytes() {
    let mut e = editor();
    let p = paragraph(&e, 1, 0);
    e.set_position(
        Position {
            paragraph: p,
            offset: 0,
        },
        false,
    );
    apply(&mut e, Edit::Insert("Правка ".into()));
    let old = e.checkpoint("Fixture.docx").unwrap();
    assert_eq!(old[15], 1);
    let (mut e, _) = Editor::from_checkpoint(&old).unwrap();
    command(&mut e, 1, 0, TableAction::InsertRow { after: true });
    apply(&mut e, Edit::Insert("Черновик новой ячейки".into()));
    apply(&mut e, Edit::Undo);
    let checkpoint = e.checkpoint("Fixture.docx").unwrap();
    assert_eq!(checkpoint[15], 2);
    let (mut restored, _) = Editor::from_checkpoint(&checkpoint).unwrap();
    apply(&mut restored, Edit::Redo);
    roundtrip(&restored);
    let current = restored.selection().focus.paragraph;
    assert!(
        restored.document().paragraphs[current]
            .text()
            .contains("Черновик")
    );
    apply(&mut restored, Edit::Undo);
    apply(&mut restored, Edit::Undo);
    assert!(
        restored.document().paragraphs[p]
            .text()
            .starts_with("Правка ")
    );
    apply(&mut restored, Edit::Undo);
    assert!(!restored.dirty());
    assert_eq!(restored.document().to_docx(None).unwrap(), FIXTURE);
}

#[test]
fn invalid_merges_and_sizes_are_rejected_before_commit() {
    let e = editor();
    let first = paragraph(&e, 0, 0);
    let end = paragraph(&e, 1, 1);
    assert!(
        e.prepare(Edit::Table(TableEdit {
            paragraph: first,
            action: TableAction::Merge { end_paragraph: end }
        }))
        .is_err()
    );
    assert!(
        e.prepare(Edit::Table(TableEdit {
            paragraph: first,
            action: TableAction::Width { twips: 1 }
        }))
        .is_err()
    );
    assert!(
        e.prepare(Edit::Table(TableEdit {
            paragraph: first,
            action: TableAction::Height { twips: u32::MAX }
        }))
        .is_err()
    );
    assert!(!e.dirty());
    assert_eq!(e.document().to_docx(None).unwrap(), FIXTURE);
}
