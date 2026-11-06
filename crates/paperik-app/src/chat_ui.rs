//! Native chat controls. The parent temporarily owns this value outside State for reentrant Win32 calls.
use crate::{
    ollama::{Client, MAX_CONTEXT, MAX_INPUT, Message},
    render,
};
use std::{
    ptr::{null, null_mut},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{Controls::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

pub const MODEL: u16 = 6001;
pub const REFRESH: u16 = 6002;
const LOG: u16 = 6003;
const INPUT: u16 = 6004;
pub const SEND: u16 = 6005;
pub const STOP: u16 = 6006;
pub const CLEAR: u16 = 6007;
pub const TOGGLE: u16 = 6008;
const STATUS: u16 = 6009;
const HINT: u16 = 6010;
const SS_LEFT: u32 = 0; // Default left-aligned STATIC style.
struct Turn {
    user: String,
    answer: String,
    complete: bool,
}
pub struct Panel {
    parent: HWND,
    controls: Vec<(u16, HWND)>,
    font: HFONT,
    caption_font: HFONT,
    dpi: f32,
    client: Client,
    models: Vec<String>,
    turns: Vec<Turn>,
    listing: bool,
    ignore: bool,
    pub chat_mode: bool,
    pub status: String,
    last_poll: Instant,
    previous_cr: bool,
    visible: bool,
    warnings: usize,
    last_layout: Option<(u32, u32, u32, usize, bool, bool)>,
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn color(rgb: [u8; 3]) -> u32 {
    u32::from(rgb[0]) | (u32::from(rgb[1]) << 8) | (u32::from(rgb[2]) << 16)
}
fn surface(dark: bool) -> [u8; 3] {
    if dark {
        [36, 46, 40]
    } else {
        render::rgb("surface")
    }
}
fn ink(dark: bool) -> [u8; 3] {
    render::rgb(if dark { "foreground" } else { "ink" })
}
pub unsafe fn control_color(dc: HDC, dark: bool) -> LRESULT {
    unsafe {
        SetTextColor(dc, color(ink(dark)));
        SetBkColor(dc, color(surface(dark)));
        SetDCBrushColor(dc, color(surface(dark)));
        GetStockObject(DC_BRUSH) as LRESULT
    }
}
pub unsafe fn draw_button(item: &DRAWITEMSTRUCT, dark: bool) {
    unsafe {
        let primary = item.CtlID == u32::from(SEND);
        let enabled = item.itemState & ODS_DISABLED == 0;
        let bg = if primary && enabled {
            render::rgb("forest")
        } else if dark {
            [49, 65, 56]
        } else {
            render::rgb("sage")
        };
        let fg = if !enabled {
            render::rgb("slate")
        } else if primary {
            [255, 255, 255]
        } else {
            ink(dark)
        };
        let background = CreateSolidBrush(color(surface(dark)));
        FillRect(item.hDC, &item.rcItem, background);
        DeleteObject(background);
        let pressed = item.itemState & ODS_SELECTED != 0;
        let bg = if pressed {
            bg.map(|channel| channel.saturating_sub(12))
        } else {
            bg
        };
        let brush = CreateSolidBrush(color(bg));
        let pen = CreatePen(PS_SOLID, 1, color(bg));
        let old_brush = SelectObject(item.hDC, brush);
        let old_pen = SelectObject(item.hDC, pen);
        let radius = (12 * windows_sys::Win32::UI::HiDpi::GetDpiForWindow(item.hwndItem).max(96)
            / 96) as i32;
        RoundRect(
            item.hDC,
            item.rcItem.left,
            item.rcItem.top,
            item.rcItem.right,
            item.rcItem.bottom,
            radius,
            radius,
        );
        SelectObject(item.hDC, old_brush);
        SelectObject(item.hDC, old_pen);
        DeleteObject(brush);
        DeleteObject(pen);
        let font = SendMessageW(item.hwndItem, WM_GETFONT, 0, 0) as HFONT;
        let old = SelectObject(item.hDC, font);
        SetBkMode(item.hDC, TRANSPARENT as i32);
        SetTextColor(item.hDC, color(fg));
        let mut text = [0u16; 128];
        let n = GetWindowTextW(item.hwndItem, text.as_mut_ptr(), text.len() as i32);
        let mut rect = item.rcItem;
        DrawTextW(
            item.hDC,
            text.as_ptr(),
            n,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        if item.itemState & ODS_FOCUS != 0 {
            let mut focus = item.rcItem;
            InflateRect(&mut focus, -3, -3);
            DrawFocusRect(item.hDC, &focus);
        }
        SelectObject(item.hDC, old);
    }
}
impl Panel {
    pub fn new(parent: HWND, dpi: f32) -> Result<Self, String> {
        Self::with_client(parent, dpi, Client::new()?)
    }
    pub(crate) fn with_client(parent: HWND, dpi: f32, client: Client) -> Result<Self, String> {
        let mut panel = Self {
            parent,
            controls: Vec::new(),
            font: null_mut(),
            caption_font: null_mut(),
            dpi: 0.0,
            client,
            models: Vec::new(),
            turns: Vec::new(),
            listing: false,
            ignore: false,
            chat_mode: true,
            status: "Выберите локальную модель".into(),
            last_poll: Instant::now(),
            previous_cr: false,
            visible: false,
            warnings: 0,
            last_layout: None,
        };
        unsafe {
            for (id, class, text, style, ex) in [
                (
                    MODEL,
                    windows_sys::w!("COMBOBOX"),
                    "",
                    CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
                    WS_EX_CLIENTEDGE,
                ),
                (
                    REFRESH,
                    windows_sys::w!("BUTTON"),
                    "Обновить",
                    BS_OWNERDRAW as u32,
                    0,
                ),
                (
                    LOG,
                    windows_sys::w!("EDIT"),
                    "",
                    ES_MULTILINE as u32 | ES_READONLY as u32 | ES_AUTOVSCROLL as u32 | WS_VSCROLL,
                    WS_EX_CLIENTEDGE,
                ),
                (
                    INPUT,
                    windows_sys::w!("EDIT"),
                    "",
                    ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32 | ES_WANTRETURN as u32 | WS_VSCROLL,
                    WS_EX_CLIENTEDGE,
                ),
                (
                    SEND,
                    windows_sys::w!("BUTTON"),
                    "Отправить",
                    BS_OWNERDRAW as u32,
                    0,
                ),
                (
                    STOP,
                    windows_sys::w!("BUTTON"),
                    "Стоп",
                    BS_OWNERDRAW as u32,
                    0,
                ),
                (
                    CLEAR,
                    windows_sys::w!("BUTTON"),
                    "Новый чат",
                    BS_OWNERDRAW as u32,
                    0,
                ),
                (
                    TOGGLE,
                    windows_sys::w!("BUTTON"),
                    "Документ",
                    BS_OWNERDRAW as u32,
                    0,
                ),
                (STATUS, windows_sys::w!("STATIC"), "", SS_LEFT, 0),
                (
                    HINT,
                    windows_sys::w!("STATIC"),
                    "Enter — отправить · Shift+Enter — строка\nДокумент не отправляется автоматически",
                    SS_LEFT,
                    0,
                ),
            ] {
                let text = wide(text);
                let handle = CreateWindowExW(
                    ex,
                    class,
                    text.as_ptr(),
                    WS_CHILD
                        | if matches!(id, STATUS | HINT) {
                            0
                        } else {
                            WS_TABSTOP
                        }
                        | style,
                    0,
                    0,
                    1,
                    1,
                    parent,
                    id as usize as HMENU,
                    null_mut(),
                    null(),
                );
                if handle.is_null() {
                    return Err("Не удалось создать панель чата".into());
                }
                panel.controls.push((id, handle));
                SetWindowTheme(handle, windows_sys::w!(""), windows_sys::w!(""));
            }
            SendMessageW(
                panel.handle(MODEL),
                CB_SETCUEBANNER,
                0,
                windows_sys::w!("Выберите модель").cast::<u16>() as isize,
            );
            SendMessageW(panel.handle(INPUT), EM_SETLIMITTEXT, 16384, 0);
            SendMessageW(panel.handle(LOG), EM_SETLIMITTEXT, 1024 * 1024, 0);
            SendMessageW(
                panel.handle(MODEL),
                CB_SETDROPPEDWIDTH,
                (320.0 * dpi / 96.0) as usize,
                0,
            );
        }
        panel.font(dpi);
        panel.update_buttons();
        panel.set_status();
        Ok(panel)
    }
    fn handle(&self, id: u16) -> HWND {
        self.controls
            .iter()
            .find(|(key, _)| *key == id)
            .map_or(null_mut(), |(_, h)| *h)
    }
    fn font(&mut self, dpi: f32) {
        if self.dpi == dpi {
            return;
        }
        unsafe {
            let create = |size: f32| {
                CreateFontW(
                    -(size * dpi / 96.0).round() as i32,
                    0,
                    0,
                    0,
                    FW_NORMAL as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET as u32,
                    OUT_DEFAULT_PRECIS as u32,
                    CLIP_DEFAULT_PRECIS as u32,
                    CLEARTYPE_QUALITY as u32,
                    DEFAULT_PITCH as u32,
                    windows_sys::w!("Segoe UI"),
                )
            };
            let font = create(14.0);
            let caption = create(12.0);
            if font.is_null() || caption.is_null() {
                if !font.is_null() {
                    DeleteObject(font);
                }
                if !caption.is_null() {
                    DeleteObject(caption);
                }
                return;
            }
            for (id, handle) in &self.controls {
                let selected = if matches!(*id, STATUS | HINT) {
                    caption
                } else {
                    font
                };
                SendMessageW(*handle, WM_SETFONT, selected as usize, 1);
            }
            SendMessageW(
                self.handle(MODEL),
                CB_SETDROPPEDWIDTH,
                (320.0 * dpi / 96.0) as usize,
                0,
            );
            if !self.font.is_null() {
                DeleteObject(self.font);
            }
            if !self.caption_font.is_null() {
                DeleteObject(self.caption_font);
            }
            self.font = font;
            self.caption_font = caption;
            self.dpi = dpi;
        }
    }
    pub fn layout(&mut self, width: f32, height: f32, dpi: f32, warnings: usize, dark: bool) {
        let key = (
            width.to_bits(),
            height.to_bits(),
            dpi.to_bits(),
            warnings,
            self.chat_mode,
            dark,
        );
        if self.last_layout == Some(key) {
            return;
        }
        self.last_layout = Some(key);
        self.font(dpi);
        self.warnings = warnings;
        let (_, right, _, _) = render::panes(width, height);
        let pane = width - right;
        self.visible = pane > 0.0 && height >= 520.0;
        let positions = [
            (TOGGLE, right + 140.0, 78.0, 124.0, 30.0),
            (MODEL, right + 12.0, 136.0, pane - 104.0, 200.0),
            (REFRESH, width - 84.0, 136.0, 72.0, 28.0),
            (STATUS, right + 12.0, 176.0, pane - 24.0, 54.0),
            (
                LOG,
                right + 12.0,
                236.0,
                pane - 24.0,
                (height - 448.0).max(50.0),
            ),
            (INPUT, right + 12.0, height - 202.0, pane - 24.0, 72.0),
            (SEND, right + 12.0, height - 120.0, 96.0, 30.0),
            (STOP, right + 114.0, height - 120.0, 50.0, 30.0),
            (CLEAR, right + 170.0, height - 120.0, pane - 182.0, 30.0),
            (HINT, right + 12.0, height - 80.0, pane - 24.0, 36.0),
        ];
        unsafe {
            for (id, x, y, w, h) in positions {
                let handle = self.handle(id);
                let visible = if id == TOGGLE {
                    pane > 0.0
                } else {
                    self.visible && self.chat_mode
                };
                if !visible && GetFocus() == handle {
                    SetFocus(self.parent);
                }
                MoveWindow(
                    handle,
                    (x * dpi / 96.0).round() as i32,
                    (y * dpi / 96.0).round() as i32,
                    (w.max(1.0) * dpi / 96.0).round() as i32,
                    (h.max(1.0) * dpi / 96.0).round() as i32,
                    1,
                );
                ShowWindow(handle, if visible { SW_SHOWNA } else { SW_HIDE });
                InvalidateRect(handle, null(), 1);
            }
            let toggle = if self.chat_mode {
                if warnings > 0 {
                    format!("Документ ({warnings})")
                } else {
                    "Документ".into()
                }
            } else {
                "Чат".into()
            };
            SetWindowTextW(self.handle(TOGGLE), wide(&toggle).as_ptr());
        }
    }
    pub fn owns(&self, handle: HWND) -> bool {
        self.controls.iter().any(|(_, h)| *h == handle)
    }
    pub fn input_handle(&self) -> HWND {
        self.handle(INPUT)
    }
    pub fn log_handle(&self) -> HWND {
        self.handle(LOG)
    }
    pub fn focus(&self) {
        if self.visible && self.chat_mode {
            unsafe {
                SetFocus(self.handle(INPUT));
            }
        }
    }
    fn selected(&self) -> Option<String> {
        let index = unsafe { SendMessageW(self.handle(MODEL), CB_GETCURSEL, 0, 0) };
        usize::try_from(index)
            .ok()
            .and_then(|i| self.models.get(i))
            .cloned()
    }
    fn set_status(&self) {
        unsafe {
            SetWindowTextW(self.handle(STATUS), wide(&self.status).as_ptr());
        }
    }
    fn update_buttons(&self) {
        unsafe {
            EnableWindow(self.handle(MODEL), (!self.client.busy()) as i32);
            EnableWindow(self.handle(REFRESH), (!self.client.busy()) as i32);
            EnableWindow(
                self.handle(SEND),
                (!self.client.busy() && self.selected().is_some()) as i32,
            );
            EnableWindow(self.handle(STOP), self.client.busy() as i32);
        }
    }
    pub fn refresh(&mut self) {
        match self.client.refresh() {
            Ok(()) => {
                self.listing = true;
                self.status = "Проверка локальной Ollama…".into();
            }
            Err(e) => self.status = e,
        }
        self.update_buttons();
        self.set_status();
    }
    fn input(&self) -> Result<String, String> {
        unsafe {
            let n = GetWindowTextLengthW(self.handle(INPUT));
            if n > 16384 {
                return Err("Сообщение слишком длинное".into());
            }
            let mut text = vec![0u16; n.max(0) as usize + 1];
            let n = GetWindowTextW(self.handle(INPUT), text.as_mut_ptr(), text.len() as i32);
            String::from_utf16(&text[..n.max(0) as usize])
                .map_err(|_| "Некорректный Unicode сообщения".into())
        }
    }
    fn append(&mut self, text: &str) {
        let mut normalized = String::new();
        for ch in text.chars() {
            match ch {
                '\r' => {
                    normalized.push_str("\r\n");
                    self.previous_cr = true;
                }
                '\n' => {
                    if !self.previous_cr {
                        normalized.push_str("\r\n");
                    }
                    self.previous_cr = false;
                }
                _ => {
                    normalized.push(ch);
                    self.previous_cr = false;
                }
            }
        }
        if normalized.is_empty() {
            return;
        }
        unsafe {
            let handle = self.handle(LOG);
            let mut start = 0u32;
            let mut end = 0u32;
            SendMessageW(
                handle,
                EM_GETSEL,
                &mut start as *mut _ as usize,
                &mut end as *mut _ as isize,
            );
            let old_len = GetWindowTextLengthW(handle) as u32;
            let first = SendMessageW(handle, EM_GETFIRSTVISIBLELINE, 0, 0);
            let follow = start == end && end >= old_len;
            SendMessageW(handle, EM_SETREADONLY, 0, 0);
            SendMessageW(handle, EM_SETSEL, old_len as usize, old_len as isize);
            SendMessageW(
                handle,
                EM_REPLACESEL,
                0,
                wide(&normalized).as_ptr() as isize,
            );
            SendMessageW(handle, EM_SETREADONLY, 1, 0);
            if follow {
                SendMessageW(handle, EM_SCROLLCARET, 0, 0);
            } else {
                SendMessageW(handle, EM_SETSEL, start as usize, end as isize);
                let now = SendMessageW(handle, EM_GETFIRSTVISIBLELINE, 0, 0);
                SendMessageW(handle, EM_LINESCROLL, 0, first - now);
            }
        }
    }
    fn submit(&mut self) -> Result<(), String> {
        if self.client.busy() {
            return Err("Дождитесь ответа или нажмите «Стоп»".into());
        }
        let model = self.selected().ok_or("Выберите локальную модель")?;
        let input = self.input()?;
        let input = input.replace("\r\n", "\n").replace('\r', "\n");
        if input.trim().is_empty() {
            return Ok(());
        }
        if input.len() > MAX_INPUT {
            return Err("Сообщение превышает 32 КиБ UTF-8".into());
        }
        if self.turns.len() >= 32 {
            return Err("Начните новый чат: достигнут предел истории".into());
        }
        if self
            .turns
            .iter()
            .map(|t| t.user.len() + t.answer.len())
            .sum::<usize>()
            + input.len()
            > MAX_CONTEXT
        {
            return Err("История слишком велика. Начните новый чат.".into());
        }
        let mut messages = Vec::new();
        for turn in self.turns.iter().filter(|t| t.complete) {
            messages.push(Message {
                role: "user",
                text: turn.user.clone(),
            });
            messages.push(Message {
                role: "assistant",
                text: turn.answer.clone(),
            });
        }
        messages.push(Message {
            role: "user",
            text: input.clone(),
        });
        if messages.iter().map(|m| m.text.len()).sum::<usize>() > MAX_CONTEXT {
            return Err("История слишком велика. Начните новый чат.".into());
        }
        self.client.send(model.clone(), messages)?;
        self.listing = false;
        self.ignore = false;
        self.turns.push(Turn {
            user: input.clone(),
            answer: String::new(),
            complete: false,
        });
        self.append(&format!("\r\nВы · {model}\r\n{input}\r\n\r\nOllama\r\n"));
        unsafe {
            SetWindowTextW(self.handle(INPUT), windows_sys::w!(""));
        }
        self.status = "Модель загружается или готовит ответ…".into();
        Ok(())
    }
    pub fn command(&mut self, id: u16) {
        match id {
            REFRESH => {
                self.refresh();
                return;
            }
            SEND => {
                if let Err(e) = self.submit() {
                    self.status = e;
                }
            }
            STOP if self.client.busy() => {
                self.client.cancel();
                self.status = "Остановка запроса…".into();
            }
            CLEAR => {
                self.client.cancel();
                self.turns.clear();
                self.ignore = self.client.busy();
                self.previous_cr = false;
                unsafe {
                    SetWindowTextW(self.handle(LOG), windows_sys::w!(""));
                }
                self.status = if self.ignore {
                    "Остановка запроса…"
                } else {
                    "Новый чат"
                }
                .into();
            }
            TOGGLE => self.chat_mode = !self.chat_mode,
            MODEL => self.status = "Локальная модель выбрана. Можно отправить сообщение.".into(),
            _ => return,
        }
        self.update_buttons();
        self.set_status();
    }
    pub fn poll(&mut self) -> bool {
        if self.last_poll.elapsed() < Duration::from_millis(50) {
            return false;
        }
        self.last_poll = Instant::now();
        let update = self.client.poll();
        let changed = !update.delta.is_empty() || update.models.is_some() || update.done.is_some();
        if let Some(models) = update.models {
            let selected = self.selected();
            self.models = models;
            unsafe {
                SendMessageW(self.handle(MODEL), CB_RESETCONTENT, 0, 0);
                for model in &self.models {
                    SendMessageW(
                        self.handle(MODEL),
                        CB_ADDSTRING,
                        0,
                        wide(model).as_ptr() as isize,
                    );
                }
                if let Some(index) =
                    selected.and_then(|name| self.models.iter().position(|m| *m == name))
                {
                    SendMessageW(self.handle(MODEL), CB_SETCURSEL, index, 0);
                }
            }
        }
        if !self.ignore && !update.delta.is_empty() {
            if let Some(turn) = self.turns.last_mut() {
                turn.answer.push_str(&update.delta);
            }
            self.append(&update.delta);
            self.status = "Ollama отвечает…".into();
        }
        if let Some(result) = update.done {
            self.status = match result {
                Ok(()) if self.ignore => "Новый чат".into(),
                Ok(()) if self.listing => {
                    if self.models.is_empty() {
                        "Локальных моделей нет. Добавьте модель в Ollama и обновите список.".into()
                    } else {
                        format!(
                            "Ollama подключена · {} локальных моделей. Выберите модель.",
                            self.models.len()
                        )
                    }
                }
                Ok(()) => {
                    if let Some(turn) = self.turns.last_mut() {
                        turn.complete = true;
                    }
                    "Ответ завершён".into()
                }
                Err(error) => {
                    if !self.listing && !self.ignore {
                        self.append(&format!("\r\n[Ответ не завершён: {error}]\r\n"));
                    }
                    error
                }
            };
            self.ignore = false;
            self.listing = false;
        }
        if changed {
            self.update_buttons();
            self.set_status();
        }
        changed
    }
}
impl Drop for Panel {
    fn drop(&mut self) {
        unsafe {
            for (_, h) in &self.controls {
                if IsWindow(*h) != 0 {
                    DestroyWindow(*h);
                }
            }
            if !self.font.is_null() {
                DeleteObject(self.font);
            }
            if !self.caption_font.is_null() {
                DeleteObject(self.caption_font);
            }
        }
    }
}
