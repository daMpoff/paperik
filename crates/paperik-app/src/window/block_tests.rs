//! Own hidden-window regression; never controls an external application.
use super::*;
use paperik_document::Block;

struct Native {
    state: Box<State>,
    handle: HWND,
    name: *const u16,
    instance: HINSTANCE,
    keys: [u8; 256],
}
impl Native {
    unsafe fn new(name: *const u16) -> Self {
        unsafe {
            let mut keys = [0; 256];
            assert_ne!(GetKeyboardState(keys.as_mut_ptr()), 0);
            assert_ne!(SetKeyboardState([0u8; 256].as_ptr()), 0);
            let instance = GetModuleHandleW(null());
            assert_ne!(
                RegisterClassW(&WNDCLASSW {
                    lpfnWndProc: Some(procedure),
                    hInstance: instance,
                    lpszClassName: name,
                    ..Default::default()
                }),
                0
            );
            let mut state = Box::new(State::empty(96.0));
            let handle = CreateWindowExW(
                0,
                name,
                TITLE,
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                1320,
                820,
                null_mut(),
                null_mut(),
                instance,
                (&mut *state as *mut State).cast(),
            );
            assert!(!handle.is_null());
            state.renderer = Some(Renderer::new(handle, 1320, 820, 96.0).unwrap());
            Self {
                state,
                handle,
                name,
                instance,
                keys,
            }
        }
    }
    fn load(&mut self, path: PathBuf) {
        self.state.load(path).unwrap();
        wait_background(self.handle).unwrap();
        assert!(
            self.state.operation_error.is_none(),
            "{:?}",
            self.state.operation_error
        );
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.handle);
            UnregisterClassW(self.name, self.instance);
            SetKeyboardState(self.keys.as_ptr());
        }
    }
}
fn index(e: &Editor) -> usize {
    e.document()
        .content
        .as_ref()
        .unwrap()
        .blocks
        .iter()
        .find_map(|b| {
            if let Block::Table(t) = b {
                t.rows
                    .iter()
                    .skip(1)
                    .flat_map(|r| &r.cells)
                    .flat_map(|c| &c.paragraphs)
                    .copied()
                    .find(|i| e.document().paragraphs[*i].edit_block.is_none())
            } else {
                None
            }
        })
        .unwrap()
}
#[test]
fn native_cell_input_reflows_recovers_and_saves_preserving_image() {
    unsafe {
        let mut native = Native::new(windows_sys::w!("Paperik.BlockEditTest"));
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/blocks.docx");
        let source = std::fs::read(&fixture).unwrap();
        let root = std::env::temp_dir().join(format!("paperik-block-edit-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let recovery_root = root.join("recovery");
        std::fs::create_dir(&recovery_root).unwrap();
        native.state.recovery_root = Some(recovery_root.clone());
        native.load(fixture.clone());
        native.state.editor.as_mut().unwrap().set_position(
            Position {
                paragraph: 2,
                offset: 0,
            },
            false,
        );
        native.state.caret_page = None;
        native.state.move_vertical(1, false);
        assert_eq!(
            native
                .state
                .editor
                .as_ref()
                .unwrap()
                .selection()
                .focus
                .paragraph,
            4
        );
        let original = native.state.editor.as_ref().unwrap().document().clone();
        let i = {
            let doc = native.state.editor.as_ref().unwrap().document();
            let Block::Table(table) = &doc.content.as_ref().unwrap().blocks[2] else {
                unreachable!()
            };
            table.rows[1].cells[1].paragraphs[0]
        };
        let (x, y) = {
            let state = &native.state;
            let doc = state.editor.as_ref().unwrap().document();
            let layout = state.layout.as_ref().unwrap();
            let (page, line) = layout
                .pages
                .iter()
                .enumerate()
                .find_map(|(p, page)| page.lines.iter().find(|l| l.paragraph == i).map(|l| (p, l)))
                .unwrap();
            let [x, _, _, _] = line.clip.unwrap();
            let (left, right, top, _) = render::panes(state.width, state.height);
            (
                (left + (right - left - doc.page.width * state.zoom) / 2.0 + (x + 3.0) * state.zoom)
                    .round() as i32,
                (top + (24.0 + page as f32 * (doc.page.height + 24.0) + line.y + line.height / 2.0
                    - state.scroll)
                    * state.zoom)
                    .round() as i32,
            )
        };
        let point = ((x as u16 as usize) | ((y as u16 as usize) << 16)) as isize;
        procedure(native.handle, WM_LBUTTONDOWN, 0, point);
        procedure(native.handle, WM_LBUTTONUP, 0, point);
        assert_eq!(
            native
                .state
                .editor
                .as_ref()
                .unwrap()
                .selection()
                .focus
                .paragraph,
            i
        );
        for unit in "Дополнение ".encode_utf16() {
            procedure(native.handle, WM_CHAR, unit as usize, 0);
        }
        wait_background(native.handle).unwrap();
        native
            .state
            .apply(Edit::Insert("\nВторая строка ".into()))
            .unwrap();
        wait_background(native.handle).unwrap();
        assert!(native.state.editor.as_ref().unwrap().dirty());
        assert_eq!(
            native
                .state
                .editor
                .as_ref()
                .unwrap()
                .document()
                .paragraphs
                .len(),
            107
        );
        let geometry = native
            .state
            .layout
            .as_ref()
            .unwrap()
            .report(native.state.editor.as_ref().unwrap().document())
            .unwrap();
        native.state.autosave_tick(true);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while native.state.autosave.as_ref().unwrap().busy {
            assert!(std::time::Instant::now() < deadline);
            native.state.autosave_tick(false);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        drop(native.state.autosave.take());
        let recovered = recovery::find(&recovery_root, &std::collections::BTreeSet::new())
            .unwrap()
            .unwrap();
        native.state.restore(recovered).unwrap();
        wait_background(native.handle).unwrap();
        assert_eq!(
            geometry,
            native
                .state
                .layout
                .as_ref()
                .unwrap()
                .report(native.state.editor.as_ref().unwrap().document())
                .unwrap()
        );
        let copy = root.join("edited.docx");
        native.state.save_to(copy.clone()).unwrap();
        wait_background(native.handle).unwrap();
        assert!(!native.state.editor.as_ref().unwrap().dirty());
        let reopened = Document::open(&copy).unwrap();
        assert_eq!(reopened.paragraphs.len(), 107);
        for part in original
            .package
            .names()
            .filter(|n| *n != "word/document.xml" && *n != "docProps/app.xml")
        {
            assert_eq!(
                original.package.bytes(part).unwrap(),
                reopened.package.bytes(part).unwrap()
            );
        }
        let paragraphs = native
            .state
            .editor
            .as_ref()
            .unwrap()
            .document()
            .paragraphs
            .clone();
        native
            .state
            .apply(Edit::Insert("Слишком высокая строка таблицы ".repeat(1000)))
            .unwrap();
        wait_background(native.handle).unwrap();
        assert!(native.state.operation_error.is_some());
        assert_eq!(
            native.state.editor.as_ref().unwrap().document().paragraphs,
            paragraphs
        );
        native.state.apply(Edit::Undo).unwrap();
        wait_background(native.handle).unwrap();
        native.state.apply(Edit::Redo).unwrap();
        wait_background(native.handle).unwrap();
        native.state.finish_autosave();
        wait_background(native.handle).unwrap();
        assert_eq!(std::fs::read(&fixture).unwrap(), source);
        std::fs::remove_file(copy).unwrap();
        std::fs::remove_dir(recovery_root).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}

#[test]
#[ignore = "manual private DOCX regression; requires explicit input and new output directory"]
fn private_block_document_edit_save() {
    let input =
        PathBuf::from(std::env::var_os("PAPERIK_TEST_DOCX").expect("set PAPERIK_TEST_DOCX"));
    let output =
        PathBuf::from(std::env::var_os("PAPERIK_TEST_OUTPUT").expect("set PAPERIK_TEST_OUTPUT"));
    std::fs::create_dir(&output).unwrap();
    let source = std::fs::read(&input).unwrap();
    unsafe {
        let mut native = Native::new(windows_sys::w!("Paperik.PrivateBlockEditTest"));
        native.load(input.clone());
        let original = native.state.editor.as_ref().unwrap().document().clone();
        let i = index(native.state.editor.as_ref().unwrap());
        native.state.editor.as_mut().unwrap().set_position(
            Position {
                paragraph: i,
                offset: 0,
            },
            false,
        );
        native
            .state
            .apply(Edit::Insert("Проверка Paperik\n".into()))
            .unwrap();
        wait_background(native.handle).unwrap();
        let before = native
            .state
            .layout
            .as_ref()
            .unwrap()
            .report(native.state.editor.as_ref().unwrap().document())
            .unwrap();
        let copy = output.join("edited.docx");
        native.state.save_to(copy.clone()).unwrap();
        wait_background(native.handle).unwrap();
        assert!(
            native.state.operation_error.is_none(),
            "{:?}",
            native.state.operation_error
        );
        let reopened = Document::open(&copy).unwrap();
        assert_eq!(
            before,
            native
                .state
                .renderer
                .as_ref()
                .unwrap()
                .document(&reopened)
                .unwrap()
                .report(&reopened)
                .unwrap()
        );
        for part in original
            .package
            .names()
            .filter(|n| *n != "word/document.xml" && *n != "docProps/app.xml")
        {
            assert_eq!(
                original.package.bytes(part).unwrap(),
                reopened.package.bytes(part).unwrap()
            );
        }
        assert_eq!(std::fs::read(&input).unwrap(), source);
        println!(
            "PRIVATE_BLOCK_EDIT paragraphs={} pages={}; copy reopened; non-text parts identical; source unchanged",
            reopened.paragraphs.len(),
            native.state.layout.as_ref().unwrap().pages.len()
        );
    }
}

#[test]
fn native_table_change_lays_out_vertical_merge_and_saves_transactionally() {
    use paperik_document::{TableAction, TableEdit, VerticalMerge};
    unsafe {
        let mut native = Native::new(windows_sys::w!("Paperik.StructureTest"));
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/blocks.docx");
        let original = std::fs::read(&fixture).unwrap();
        native.load(fixture.clone());
        let (start, end) = {
            let doc = native.state.editor.as_ref().unwrap().document();
            let Block::Table(table) = &doc.content.as_ref().unwrap().blocks[2] else {
                unreachable!()
            };
            (
                table.rows[1].cells[0].paragraphs[0],
                table.rows[2].cells[0].paragraphs[0],
            )
        };
        edit(
            native.handle,
            Edit::Table(TableEdit {
                paragraph: start,
                action: TableAction::Merge { end_paragraph: end },
            }),
        );
        wait_background(native.handle).unwrap();
        assert!(
            native.state.operation_error.is_none(),
            "{:?}",
            native.state.operation_error
        );
        let editor = native.state.editor.as_ref().unwrap();
        let document = editor.document();
        let Block::Table(table) = &document.content.as_ref().unwrap().blocks[2] else {
            unreachable!()
        };
        let cell = &table.rows[1].cells[0];
        assert_eq!(cell.merge, Some(VerticalMerge::Start));
        assert_eq!(cell.paragraphs.len(), 2);
        let first = cell.paragraphs[0];
        let second = cell.paragraphs[1];
        let layout = native.state.layout.as_ref().unwrap();
        let (page, line) = layout
            .pages
            .iter()
            .enumerate()
            .find_map(|(p, page)| {
                page.lines
                    .iter()
                    .find(|line| line.paragraph == second)
                    .map(|line| (p, line))
            })
            .unwrap();
        let [x, _, w, _] = line.clip.unwrap();
        assert_eq!(
            layout
                .hit_test(document, page, x + w / 2.0, line.y + line.height / 2.0)
                .unwrap()
                .unwrap()
                .0
                .paragraph,
            second
        );
        let first_line = layout.pages[page]
            .lines
            .iter()
            .find(|line| line.paragraph == first)
            .unwrap();
        assert_eq!(first_line.clip, line.clip);
        assert!(
            layout.pages[page]
                .cells
                .iter()
                .any(|cell| cell.rect[3] > cell.row_height)
        );
        let content = document.content.clone();
        edit(
            native.handle,
            Edit::Table(TableEdit {
                paragraph: first,
                action: TableAction::Height { twips: 150_000 },
            }),
        );
        wait_background(native.handle).unwrap();
        assert!(native.state.operation_error.is_some());
        assert_eq!(
            native.state.editor.as_ref().unwrap().document().content,
            content
        );
        let root =
            std::env::temp_dir().join(format!("paperik-table-structure-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let output = root.join("merged.docx");
        native.state.save_to(output.clone()).unwrap();
        wait_background(native.handle).unwrap();
        assert_eq!(Document::open(&output).unwrap().content, content);
        assert_eq!(std::fs::read(&fixture).unwrap(), original);
        std::fs::remove_file(output).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}

#[test]
fn native_picture_file_edit_resize_delete_and_save() {
    use paperik_document::{PictureAction, PictureEdit};
    unsafe {
        let mut native = Native::new(windows_sys::w!("Paperik.PictureTest"));
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/blocks.docx");
        native.load(fixture);
        let root =
            std::env::temp_dir().join(format!("paperik-picture-edit-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let raster = root.join("synthetic.png");
        let image = native
            .state
            .editor
            .as_ref()
            .unwrap()
            .document()
            .package
            .bytes("word/media/sample.png")
            .unwrap();
        std::fs::write(&raster, &image).unwrap();
        let input = raster.clone();
        native
            .state
            .apply_with(move |doc| picture::file_edit(doc, 0, input, false, None))
            .unwrap();
        wait_background(native.handle).unwrap();
        assert!(
            native.state.operation_error.is_none(),
            "{:?}",
            native.state.operation_error
        );
        let editor = native.state.editor.as_ref().unwrap();
        let p = editor.selection().focus.paragraph;
        let placed = &native.state.layout.as_ref().unwrap().pages[0].images;
        assert_eq!(placed.len(), 2);
        let [x, y, width, height] = placed[0].rect;
        assert_eq!([width, height], [32.0, 16.0]);
        assert_eq!(placed[0].paragraph, p);
        let (left, right, top, _) = render::panes(native.state.width, native.state.height);
        let page_x = left + (right - left - editor.document().page.width * native.state.zoom) / 2.0;
        let page_y = top + (24.0 - native.state.scroll) * native.state.zoom;
        let (hit, _, _) = native
            .state
            .hit(
                page_x + (x + width / 2.0) * native.state.zoom,
                page_y + (y + height / 2.0) * native.state.zoom,
            )
            .unwrap();
        assert_eq!(hit.paragraph, p);

        let command = |action| {
            Edit::Picture(PictureEdit {
                paragraph: p,
                offset: None,
                action,
            })
        };
        edit(
            native.handle,
            command(PictureAction::Resize { size: [960, 480] }),
        );
        wait_background(native.handle).unwrap();
        assert_eq!(
            &native.state.layout.as_ref().unwrap().pages[0].images[0].rect[2..],
            &[64.0, 32.0]
        );
        let resized = native.state.editor.as_ref().unwrap().revision();

        edit(
            native.handle,
            command(PictureAction::Replace {
                bytes: std::sync::Arc::new(b"\x89PNG\r\n\x1a\ninvalid".to_vec()),
            }),
        );
        wait_background(native.handle).unwrap();
        assert!(native.state.operation_error.is_some());
        assert_eq!(native.state.editor.as_ref().unwrap().revision(), resized);

        edit(native.handle, command(PictureAction::Delete));
        wait_background(native.handle).unwrap();
        assert_eq!(
            native.state.layout.as_ref().unwrap().pages[0].images.len(),
            1
        );
        edit(native.handle, Edit::Undo);
        wait_background(native.handle).unwrap();
        assert_eq!(
            native.state.layout.as_ref().unwrap().pages[0].images.len(),
            2
        );
        let output = root.join("edited.docx");
        native.state.save_to(output.clone()).unwrap();
        wait_background(native.handle).unwrap();
        let reopened = Document::open(&output).unwrap();
        assert_eq!(
            reopened.content,
            native.state.editor.as_ref().unwrap().document().content
        );
        std::fs::remove_file(raster).unwrap();
        std::fs::remove_file(output).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}

#[test]
fn native_inline_picture_has_caret_geometry_and_expands_the_table_line() {
    use paperik_document::{PictureAction, PictureEdit};
    unsafe {
        let mut native = Native::new(windows_sys::w!("Paperik.InlinePictureTest"));
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/blocks.docx");
        native.load(fixture);
        let editor = native.state.editor.as_ref().unwrap();
        let p = index(editor);
        let bytes = std::sync::Arc::new(
            editor
                .document()
                .package
                .bytes("word/media/sample.png")
                .unwrap(),
        );
        let insert = |paragraph, offset, size| {
            Edit::Picture(PictureEdit {
                paragraph,
                offset: Some(offset),
                action: PictureAction::Insert {
                    bytes: bytes.clone(),
                    size,
                },
            })
        };
        edit(native.handle, insert(0, 3, [600, 450]));
        wait_background(native.handle).unwrap();
        assert!(
            native.state.operation_error.is_none(),
            "{:?}",
            native.state.operation_error
        );
        edit(native.handle, insert(p, 2, [900, 600]));
        wait_background(native.handle).unwrap();
        assert!(
            native.state.operation_error.is_none(),
            "{:?}",
            native.state.operation_error
        );
        let editor = native.state.editor.as_ref().unwrap();
        let layout = native.state.layout.as_ref().unwrap();
        for (paragraph, offset) in [(0, 3), (p, 2)] {
            let (page_no, image) = layout
                .pages
                .iter()
                .enumerate()
                .find_map(|(i, page)| {
                    page.images
                        .iter()
                        .find(|p| p.paragraph == paragraph && p.offset == Some(offset))
                        .map(|p| (i, p))
                })
                .unwrap();
            let [x, y, w, h] = image.rect;
            let line = layout.pages[page_no]
                .lines
                .iter()
                .find(|line| {
                    line.paragraph == paragraph
                        && (line.text_start..line.text_start + line.text_length).contains(&offset)
                })
                .unwrap();
            assert!((line.y + line.baseline - y - h).abs() < 0.01);
            assert!(line.height >= h);
            for (dx, expected) in [(w * 0.2, offset), (w * 0.8, offset + 1)] {
                let hit = layout
                    .hit_test(editor.document(), page_no, x + dx, y + h * 0.5)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    hit.0,
                    Position {
                        paragraph,
                        offset: expected
                    }
                );
            }
            if paragraph == p {
                let [a, b, cw, ch] = line.clip.unwrap();
                assert!(x >= a && y >= b && x + w <= a + cw && y + h <= b + ch);
            }
        }
    }
}
