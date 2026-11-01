//! Plain Unicode text only. Clipboard ownership changes only after successful allocation.
use windows_sys::Win32::{
    Foundation::{GlobalFree, HWND},
    System::{DataExchange::*, Memory::*},
};
const UNICODE_TEXT: u32 = 13;
const MAX_BYTES: usize = 32 * 1024 * 1024;
struct Clipboard;
impl Drop for Clipboard {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}
pub fn read(window: HWND) -> Result<String, String> {
    unsafe {
        if OpenClipboard(window) == 0 {
            return Err("Буфер обмена занят другим приложением".into());
        }
        let _clipboard = Clipboard;
        if IsClipboardFormatAvailable(UNICODE_TEXT) == 0 {
            return Err("В буфере обмена нет текста".into());
        }
        let data = GetClipboardData(UNICODE_TEXT);
        if data.is_null() {
            return Err("Не удалось прочитать буфер обмена".into());
        }
        let size = GlobalSize(data);
        if size == 0 || size > MAX_BYTES || !size.is_multiple_of(2) {
            return Err("Неподдерживаемый размер текста в буфере обмена".into());
        }
        let pointer = GlobalLock(data).cast::<u16>();
        if pointer.is_null() {
            return Err("Не удалось прочитать текст буфера обмена".into());
        }
        let units = std::slice::from_raw_parts(pointer, size / 2);
        let result = units
            .iter()
            .position(|u| *u == 0)
            .ok_or("Текст буфера обмена не завершён".into())
            .and_then(|end| {
                String::from_utf16(&units[..end])
                    .map_err(|_| "Текст буфера обмена содержит повреждённый Unicode".into())
            });
        GlobalUnlock(data);
        result
    }
}
pub fn write(window: HWND, text: &str) -> Result<(), String> {
    let units: Vec<_> = text.encode_utf16().chain(Some(0)).collect();
    let size = units.len() * 2;
    if size > MAX_BYTES {
        return Err("Выделение слишком велико для буфера обмена".into());
    }
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, size);
        if memory.is_null() {
            return Err("Не хватает памяти для копирования".into());
        }
        let pointer = GlobalLock(memory).cast::<u16>();
        if pointer.is_null() {
            GlobalFree(memory);
            return Err("Не удалось подготовить буфер обмена".into());
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), pointer, units.len());
        GlobalUnlock(memory);
        if OpenClipboard(window) == 0 {
            GlobalFree(memory);
            return Err("Буфер обмена занят другим приложением".into());
        }
        let _clipboard = Clipboard;
        if EmptyClipboard() == 0 || SetClipboardData(UNICODE_TEXT, memory).is_null() {
            GlobalFree(memory);
            return Err("Не удалось скопировать выделение".into());
        }
        // Windows now owns the allocation.
        Ok(())
    }
}
