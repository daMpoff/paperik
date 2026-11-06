use super::*;
use paperik_document::{Block, PictureAction, PictureEdit};
use std::{io::Read, sync::Arc};

const INSERT: usize = 1;
const INSERT_BLOCK: usize = 6;
const REPLACE: usize = 2;
const WIDTH: usize = 3;
const HEIGHT: usize = 4;
const DELETE: usize = 5;
const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

struct Target {
    position: Position,
    image: Option<(Option<u32>, [f32; 2])>,
    stamp: u64,
    document: *const Document,
}

fn target(
    state: &State,
    position: Position,
    image: Option<(Option<u32>, [f32; 2])>,
) -> Option<Target> {
    if state.busy() || state.closing || state.composing || state.after.is_some() {
        return None;
    }
    let editor = state.editor.as_ref()?;
    let doc = editor.document();
    doc.paragraphs.get(position.paragraph)?;
    Some(Target {
        position,
        image,
        stamp: editor.checkpoint_stamp(),
        document: doc,
    })
}

impl Target {
    fn valid(&self, state: &State) -> bool {
        !state.busy()
            && !state.closing
            && !state.composing
            && state.after.is_none()
            && state.editor.as_ref().is_some_and(|editor| {
                std::ptr::eq(editor.document(), self.document)
                    && editor.checkpoint_stamp() == self.stamp
            })
    }
}

pub(super) unsafe fn context(window: HWND, lparam: LPARAM, image_only: bool) -> bool {
    unsafe {
        let pointer = state_pointer(window);
        if pointer.is_null() {
            return false;
        }
        let state = &*pointer;
        let keyboard = lparam == -1;
        let mut point = POINT {
            x: lparam as u16 as i16 as i32,
            y: (lparam >> 16) as u16 as i16 as i32,
        };
        let (position, image) = if keyboard {
            let Some(editor) = state.editor.as_ref() else {
                return false;
            };
            let position = editor.selection().focus;
            let image = state
                .layout
                .iter()
                .flat_map(|l| &l.pages)
                .flat_map(|p| &p.images)
                .find(|image| {
                    image.paragraph == position.paragraph
                        && image.offset.unwrap_or(0) == position.offset
                })
                .map(|image| (image.offset, [image.rect[2], image.rect[3]]));
            if GetCaretPos(&mut point) == 0 {
                let (left, right, top, bottom) = render::panes(state.width, state.height);
                let dpi = state.dpi / 96.0;
                point.x = ((left + right) / 2.0 * dpi).round() as i32;
                point.y = ((top + bottom) / 2.0 * dpi).round() as i32;
            }
            if ClientToScreen(window, &mut point) == 0 {
                return false;
            }
            (position, image)
        } else {
            let mut local = point;
            if ScreenToClient(window, &mut local) == 0 {
                return false;
            }
            let x = local.x as f32 * 96.0 / state.dpi;
            let y = local.y as f32 * 96.0 / state.dpi;
            let (left, right, top, bottom) = render::panes(state.width, state.height);
            if !(left..right).contains(&x) || !(top..bottom).contains(&y) {
                return false;
            }
            let Some((hit, _, _)) = state.hit(x, y) else {
                return false;
            };
            let image = state
                .page_point(x, y)
                .and_then(|(page, x, y)| {
                    state
                        .layout
                        .as_ref()?
                        .pages
                        .get(page)?
                        .images
                        .iter()
                        .find(|image| image.contains(x, y))
                })
                .map(|image| (image.offset, [image.rect[2], image.rect[3]]));
            (hit, image)
        };
        if image_only != image.is_some() {
            return false;
        }
        let Some(target) = target(state, position, image) else {
            return false;
        };
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return false;
        }
        AppendMenuW(
            menu,
            MF_STRING,
            INSERT,
            windows_sys::w!("Вставить рисунок в строку…"),
        );
        if matches!(target.image, Some((None, _))) {
            EnableMenuItem(menu, INSERT as u32, MF_BYCOMMAND | MF_GRAYED);
        }
        AppendMenuW(
            menu,
            MF_STRING,
            INSERT_BLOCK,
            windows_sys::w!("Вставить рисунок после блока…"),
        );
        if target.image.is_some() {
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            AppendMenuW(
                menu,
                MF_STRING,
                REPLACE,
                windows_sys::w!("Заменить рисунок…"),
            );
            AppendMenuW(
                menu,
                MF_STRING,
                WIDTH,
                windows_sys::w!("Изменить ширину рисунка…"),
            );
            AppendMenuW(
                menu,
                MF_STRING,
                HEIGHT,
                windows_sys::w!("Изменить высоту рисунка…"),
            );
            AppendMenuW(menu, MF_STRING, DELETE, windows_sys::w!("Удалить рисунок"));
        }
        let choice = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            0,
            window,
            null(),
        );
        DestroyMenu(menu);
        match choice as usize {
            INSERT => choose_file(window, target, false, true),
            INSERT_BLOCK => choose_file(window, target, false, false),
            REPLACE => choose_file(window, target, true, false),
            DELETE => apply(window, target, PictureAction::Delete),
            WIDTH | HEIGHT => {
                let axis = usize::from(choice as usize == HEIGHT);
                let title = if axis == 0 {
                    "Ширина рисунка"
                } else {
                    "Высота рисунка"
                };
                if let Some((_, size)) = target.image
                    && let Some(value) =
                        table::ask_size(window, title, (size[axis] * 15.0).round() as u32)
                {
                    let next = proportional_size(size, axis, value);
                    apply(window, target, PictureAction::Resize { size: next });
                }
            }
            _ => {}
        }
        true
    }
}

fn proportional_size(size: [f32; 2], axis: usize, value: u32) -> [u32; 2] {
    let mut result = size.map(|n| {
        (f64::from(n) / f64::from(size[axis]) * f64::from(value))
            .round()
            .max(1.0) as u32
    });
    result[axis] = value;
    result
}

pub(super) unsafe fn insert_at_caret(window: HWND) {
    unsafe {
        let pointer = state_pointer(window);
        if !pointer.is_null()
            && let Some(editor) = (&*pointer).editor.as_ref()
        {
            let position = editor.selection().focus;
            insert_inline(window, position);
        }
    }
}

pub(super) unsafe fn insert_inline(window: HWND, position: Position) {
    unsafe {
        let pointer = state_pointer(window);
        if !pointer.is_null()
            && let Some(target) = target(&*pointer, position, None)
        {
            choose_file(window, target, false, true);
        }
    }
}

unsafe fn apply(window: HWND, target: Target, action: PictureAction) {
    unsafe {
        let pointer = state_pointer(window);
        if !pointer.is_null() && target.valid(&*pointer) {
            edit(
                window,
                Edit::Picture(PictureEdit {
                    paragraph: target.position.paragraph,
                    offset: target.image.and_then(|(offset, _)| offset),
                    action,
                }),
            );
        }
    }
}

unsafe fn choose_file(window: HWND, target: Target, replace: bool, in_line: bool) {
    let Some(path) = open_file_dialog(
        window,
        windows_sys::w!("Рисунки PNG и JPEG (*.png;*.jpg;*.jpeg)\0*.png;*.jpg;*.jpeg\0\0"),
    ) else {
        return;
    };
    unsafe {
        let pointer = state_pointer(window);
        if pointer.is_null() || !target.valid(&*pointer) {
            return;
        }
        let paragraph = target.position.paragraph;
        let offset = if replace {
            target.image.and_then(|(offset, _)| offset)
        } else {
            in_line.then_some(target.position.offset)
        };
        if let Err(error) =
            (&mut *pointer).apply_with(move |doc| file_edit(doc, paragraph, path, replace, offset))
        {
            (&mut *pointer).operation_error = Some(error.clone());
            (&mut *pointer).message = error;
        }
        refresh(window, true);
    }
}

pub(super) fn file_edit(
    doc: &Document,
    paragraph: usize,
    path: PathBuf,
    replace: bool,
    offset: Option<u32>,
) -> Result<Edit, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(MAX_IMAGE_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("Не удалось прочитать рисунок: {e}"))?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err("Рисунок превышает предел 16 МиБ".into());
    }
    if !(bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.starts_with(b"\xff\xd8\xff")) {
        return Err("Выберите рисунок PNG или JPEG".into());
    }
    let bytes = Arc::new(bytes);
    let action = if replace {
        PictureAction::Replace { bytes }
    } else {
        let [width, height] = crate::layout::picture_dimensions(&bytes)
            .map_err(|e| format!("Не удалось прочитать размеры рисунка: {e}"))?;
        let width = width as f32;
        let height = height as f32;
        let page = &doc.page;
        let mut max_width = page.width - page.left - page.right;
        if offset.is_some() {
            if let Some((block, row, cell)) = doc.table_cell(paragraph)
                && let Block::Table(table) = &doc.content.as_ref().unwrap().blocks[block]
            {
                let cell = &table.rows[row].cells[cell];
                max_width = table.columns[cell.column..cell.column + cell.span]
                    .iter()
                    .sum::<f32>()
                    - cell.margins[1]
                    - cell.margins[3];
            }
            let style = &doc.paragraphs[paragraph].style;
            max_width -= style.left + style.right + style.first_line.max(0.0);
        }
        let max_height = page.height - page.top - page.bottom - 16.0;
        if max_width < 1.0 || max_height < 1.0 {
            return Err("Для рисунка нет места в области текста".into());
        }
        let scale = 1.0_f32.min(max_width / width).min(max_height / height);
        let size = [width, height].map(|pixels| (pixels * scale * 15.0).floor().max(1.0) as u32);
        PictureAction::Insert { bytes, size }
    };
    Ok(Edit::Picture(PictureEdit {
        paragraph,
        offset,
        action,
    }))
}
