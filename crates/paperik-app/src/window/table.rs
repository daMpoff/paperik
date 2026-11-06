use super::*;
use paperik_document::{Block, TableAction, TableEdit, VerticalMerge};
use windows_sys::Win32::UI::Controls::{EM_SETLIMITTEXT, EM_SETSEL};

enum Command {
    InsertPicture,
    Edit(TableAction),
    Size { width: bool },
    Merge { start: usize, end: usize },
}

struct Target {
    position: Position,
    width: u32,
    height: u32,
    stamp: u64,
    document: *const Document,
    label: String,
    merge: Option<(usize, usize, String)>,
    split: bool,
}

fn target(state: &State, mouse: Option<POINT>) -> Option<(Target, POINT)> {
    if state.busy() || state.closing || state.composing {
        return None;
    }
    let editor = state.editor.as_ref()?;
    let document = editor.document();
    let layout = state.layout.as_ref()?;
    let (left, right, top, bottom) = render::panes(state.width, state.height);
    let scale = state.dpi / 96.0;
    let (position, page, x, y, point) = if let Some(point) = mouse {
        let x = point.x as f32 / scale;
        let y = point.y as f32 / scale;
        if !(left..right).contains(&x) || !(top..bottom).contains(&y) {
            return None;
        }
        let (page, x, y) = state.page_point(x, y)?;
        let (position, _) = layout.hit_test(document, page, x, y).ok().flatten()?;
        (position, page, x, y, point)
    } else {
        let caret = state.caret()?;
        let [x, y, w, h] = layout.pages[caret.page].lines[caret.line].clip?;
        let page_x = left + (right - left - document.page.width * state.zoom) / 2.0;
        let page_y = top
            + (24.0 + caret.page as f32 * (document.page.height + 24.0) - state.scroll)
                * state.zoom;
        let point = POINT {
            x: ((page_x + caret.x * state.zoom).clamp(left, right) * scale).round() as i32,
            y: ((page_y + (caret.y + caret.height) * state.zoom).clamp(top, bottom) * scale).round()
                as i32,
        };
        (
            editor.selection().focus,
            caret.page,
            x + w / 2.0,
            y + h / 2.0,
            point,
        )
    };
    let (block, row, cell) = document.table_cell(position.paragraph)?;
    let Block::Table(table) = &document.content.as_ref()?.blocks[block] else {
        return None;
    };
    let cell = &table.rows[row].cells[cell];
    let rect = layout.pages[page].cells.iter().find(|cell| {
        let [a, b, w, h] = cell.rect;
        (a..a + w).contains(&x) && (b..b + h).contains(&y)
    })?;
    let selection = editor.selection();
    let merge = document
        .table_cell(selection.anchor.paragraph)
        .zip(document.table_cell(selection.focus.paragraph))
        .and_then(|((a, row_a, col_a), (b, row_b, col_b))| {
            if a != block || b != block || (row_a == row_b && col_a == col_b) {
                return None;
            }
            let a = &table.rows[row_a].cells[col_a];
            let b = &table.rows[row_b].cells[col_b];
            let rows = (row_a.min(row_b), row_a.max(row_b));
            let columns = (
                a.column.min(b.column),
                (a.column + a.span).max(b.column + b.span),
            );
            let label = format!(
                "Объединить ячейки (стр. {}–{}, ст. {}–{})",
                rows.0 + 1,
                rows.1 + 1,
                columns.0 + 1,
                columns.1
            );
            Some((selection.anchor.paragraph, selection.focus.paragraph, label))
        });
    let width = (rect.rect[2] * 15.0).round() as u32;
    let height = (rect.row_height * 15.0).round() as u32;
    let label = format!("Строка {}, столбец {}", row + 1, cell.column + 1);
    Some((
        Target {
            position,
            width,
            height,
            stamp: editor.checkpoint_stamp(),
            document,
            label,
            merge,
            split: cell.span > 1 || cell.merge == Some(VerticalMerge::Start),
        },
        point,
    ))
}

pub(super) unsafe fn context(window: HWND, lparam: LPARAM) -> bool {
    unsafe {
        let state = state_pointer(window);
        if state.is_null() {
            return false;
        }
        let keyboard = lparam == -1;
        let mut point = POINT {
            x: lparam as u16 as i16 as i32,
            y: (lparam >> 16) as u16 as i16 as i32,
        };
        if !keyboard && ScreenToClient(window, &mut point) == 0 {
            return false;
        }
        let Some((target, mut point)) = target(&*state, (!keyboard).then_some(point)) else {
            return false;
        };
        if ClientToScreen(window, &mut point) == 0 {
            return false;
        }
        let Some(command) = popup(window, point, &target) else {
            return true;
        };
        let valid = || {
            let state = state_pointer(window);
            !state.is_null()
                && !(&*state).busy()
                && (&*state).editor.as_ref().is_some_and(|editor| {
                    std::ptr::eq(editor.document(), target.document)
                        && editor.checkpoint_stamp() == target.stamp
                })
        };
        let (paragraph, action) = match command {
            Command::InsertPicture => {
                if valid() {
                    picture::insert_inline(window, target.position);
                }
                return true;
            }
            Command::Edit(action) => (target.position.paragraph, action),
            Command::Merge { start, end } => (start, TableAction::Merge { end_paragraph: end }),
            Command::Size { width } => {
                let (title, current) = if width {
                    ("Ширина ячейки", target.width)
                } else {
                    ("Минимальная высота строки", target.height)
                };
                let Some(twips) = ask_size(window, title, current) else {
                    return true;
                };
                (
                    target.position.paragraph,
                    if width {
                        TableAction::Width { twips }
                    } else {
                        TableAction::Height { twips }
                    },
                )
            }
        };
        if valid() {
            edit(window, Edit::Table(TableEdit { paragraph, action }));
        }
        true
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

unsafe fn popup(window: HWND, point: POINT, target: &Target) -> Option<Command> {
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return None;
        }
        AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, wide(&target.label).as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        let mut commands = Vec::new();
        let mut add = |label: &str, command: Command| {
            commands.push(command);
            AppendMenuW(menu, MF_STRING, commands.len(), wide(label).as_ptr());
        };
        add(
            "Вставить строку выше",
            Command::Edit(TableAction::InsertRow { after: false }),
        );
        add(
            "Вставить строку ниже",
            Command::Edit(TableAction::InsertRow { after: true }),
        );
        add("Удалить строку", Command::Edit(TableAction::DeleteRow));
        add(
            "Вставить столбец слева",
            Command::Edit(TableAction::InsertColumn { after: false }),
        );
        add(
            "Вставить столбец справа",
            Command::Edit(TableAction::InsertColumn { after: true }),
        );
        add("Удалить столбец", Command::Edit(TableAction::DeleteColumn));
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        add("Вставить рисунок в ячейку…", Command::InsertPicture);
        add("Ширина ячейки…", Command::Size { width: true });
        add("Минимальная высота строки…", Command::Size { width: false });
        add(
            "Высота по содержимому",
            Command::Edit(TableAction::AutoHeight),
        );
        if let Some((start, end, label)) = &target.merge {
            add(
                label,
                Command::Merge {
                    start: *start,
                    end: *end,
                },
            );
        } else {
            AppendMenuW(
                menu,
                MF_STRING | MF_GRAYED,
                0,
                windows_sys::w!("Выделите ячейки для объединения"),
            );
        }
        if target.split {
            add("Разъединить ячейку", Command::Edit(TableAction::Split));
        }
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            0,
            window,
            null(),
        );
        DestroyMenu(menu);
        (chosen > 0)
            .then(|| commands.into_iter().nth(chosen as usize - 1))
            .flatten()
    }
}

const SIZE_EDIT: u16 = 100;
const TWIPS_PER_MM: f64 = 1440.0 / 25.4;

pub(super) fn ask_size(parent: HWND, title: &str, current: u32) -> Option<u32> {
    let aligned = size_template(title);
    let result = unsafe {
        DialogBoxIndirectParamW(
            GetModuleHandleW(null()),
            aligned.as_ptr().cast(),
            parent,
            Some(size_proc),
            current as isize,
        )
    };
    (result > 0).then_some(result as u32)
}

fn size_template(title: &str) -> Vec<u32> {
    fn dword(words: &mut Vec<u16>, value: u32) {
        words.extend([value as u16, (value >> 16) as u16]);
    }
    let mut words = Vec::new();
    dword(
        &mut words,
        WS_POPUP | WS_CAPTION | WS_SYSMENU | (DS_MODALFRAME | DS_CENTER | DS_SETFONT) as u32,
    );
    dword(&mut words, 0);
    words.extend([4, 0, 0, 180, 65, 0, 0]);
    words.extend(wide(title));
    words.push(9);
    words.extend(wide("Segoe UI"));
    let controls = [
        (0x0082, "Размер, мм", [8, 10, 62, 14, 0xffff], 0),
        (
            0x0081,
            "",
            [74, 8, 98, 14, SIZE_EDIT],
            WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32,
        ),
        (
            0x0080,
            "Применить",
            [52, 42, 59, 15, IDOK as u16],
            WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
        ),
        (
            0x0080,
            "Отмена",
            [115, 42, 57, 15, IDCANCEL as u16],
            WS_TABSTOP,
        ),
    ];
    for (class, title, bounds, style) in controls {
        if words.len() % 2 != 0 {
            words.push(0);
        }
        dword(&mut words, WS_CHILD | WS_VISIBLE | style);
        dword(&mut words, 0);
        words.extend(bounds);
        words.extend([0xffff, class]);
        words.extend(wide(title));
        words.push(0);
    }
    // Dialog templates and control records are aligned on DWORD boundaries.
    words
        .chunks(2)
        .map(|w| u32::from(w[0]) | (u32::from(*w.get(1).unwrap_or(&0)) << 16))
        .collect()
}

fn parse_size(value: &str) -> Option<u32> {
    let mm = value.trim().replace(',', ".").parse::<f64>().ok()?;
    let twips = (mm * TWIPS_PER_MM).round();
    (1.0..=150_000.0).contains(&twips).then_some(twips as u32)
}

unsafe extern "system" fn size_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    unsafe {
        match message {
            WM_INITDIALOG => {
                let field = GetDlgItem(window, i32::from(SIZE_EDIT));
                let value = format!("{:.2}", lparam as f64 / TWIPS_PER_MM).replace('.', ",");
                SetWindowTextW(field, wide(&value).as_ptr());
                SendMessageW(field, EM_SETLIMITTEXT, 15, 0);
                SetFocus(field);
                SendMessageW(field, EM_SETSEL, 0, -1);
                0
            }
            WM_COMMAND if wparam as u16 == IDOK as u16 => {
                let mut buffer = [0u16; 16];
                let len = GetDlgItemTextW(
                    window,
                    i32::from(SIZE_EDIT),
                    buffer.as_mut_ptr(),
                    buffer.len() as i32,
                );
                if let Some(value) = parse_size(&String::from_utf16_lossy(&buffer[..len as usize]))
                {
                    EndDialog(window, value as isize);
                } else {
                    MessageBoxW(
                        window,
                        windows_sys::w!("Введите положительный размер в миллиметрах"),
                        windows_sys::w!("Размер"),
                        MB_OK | MB_ICONINFORMATION,
                    );
                }
                1
            }
            WM_CLOSE => {
                EndDialog(window, 0);
                1
            }
            WM_COMMAND if wparam as u16 == IDCANCEL as u16 => {
                EndDialog(window, 0);
                1
            }
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_size_template_creates_a_decimal_edit_and_buttons() {
        unsafe {
            let template = size_template("Размер ячейки");
            let dialog = CreateDialogIndirectParamW(
                GetModuleHandleW(null()),
                template.as_ptr().cast(),
                null_mut(),
                Some(size_proc),
                1440,
            );
            assert!(!dialog.is_null());
            let mut input = [0u16; 16];
            let count = GetDlgItemTextW(
                dialog,
                i32::from(SIZE_EDIT),
                input.as_mut_ptr(),
                input.len() as i32,
            );
            assert_eq!(
                parse_size(&String::from_utf16_lossy(&input[..count as usize])),
                Some(1440)
            );
            assert!(!GetDlgItem(dialog, IDOK).is_null());
            assert!(!GetDlgItem(dialog, IDCANCEL).is_null());
            assert_eq!(parse_size("12,5"), parse_size("12.5"));
            assert_eq!(parse_size("0"), None);
            DestroyWindow(dialog);
        }
    }
}
