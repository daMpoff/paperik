use super::*;
use crate::tasks::{self, Background};
use std::{collections::BTreeSet, sync::atomic::AtomicBool};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Load,
    Edit,
    Save,
    Scan,
    Restore,
}
pub(super) enum Action {
    Open(PathBuf),
    Save,
    Close,
    Scan(bool),
    Restore(Box<Recovered>),
}
pub(super) struct ScanResult {
    pub candidate: Option<Recovered>,
    pub startup: bool,
    pub skip: BTreeSet<PathBuf>,
}
pub(super) struct Installed {
    editor: Editor,
    layout: Layout,
    filename: String,
    path: Option<PathBuf>,
    autosave: Option<Autosave>,
    error: Option<String>,
    restored: bool,
    message: String,
}
pub(super) enum Outcome {
    Installed(Box<Installed>),
    Edited(Editor, Option<Layout>, u64),
    Saved(PathBuf, u64),
    Found(Box<ScanResult>),
}

fn paginate(editor: &Editor, cancel: &AtomicBool) -> Result<Layout, String> {
    tasks::check(cancel)?;
    // DWrite interfaces are Send + Sync in windows-rs; no D2D brushes are attached here.
    let factory: IDWriteFactory =
        unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.map_err(|e| e.to_string())?;
    crate::layout::build_cancelled(&factory, editor.document(), cancel)
        .map_err(|e| format!("Не удалось выполнить вёрстку: {e}"))
}
fn autosave(root: Option<PathBuf>) -> (Option<Autosave>, Option<String>) {
    match root.map(|root| Autosave::new(&root)).transpose() {
        Ok(value) => (value, None),
        Err(error) => (None, Some(error)),
    }
}
impl State {
    pub(super) fn busy(&self) -> bool {
        self.work.as_ref().is_some_and(Background::busy)
    }
    fn start(
        &mut self,
        kind: Kind,
        task: impl FnOnce(&AtomicBool) -> Result<Outcome, String> + Send + 'static,
    ) -> Result<(), String> {
        if self.work.is_none() {
            self.work = Some(Background::new()?);
        }
        self.work.as_mut().unwrap().start(task)?;
        self.kind = Some(kind);
        self.message = match kind {
            Kind::Load => "Открытие документа… Esc — отменить",
            Kind::Edit => "Пересчёт документа…",
            Kind::Save => "Сохранение копии…",
            Kind::Scan => "Поиск сессий восстановления… Esc — отменить",
            Kind::Restore => "Восстановление документа… Esc — отменить",
        }
        .into();
        Ok(())
    }
    pub(super) fn load(&mut self, path: PathBuf) -> Result<(), String> {
        let root = self.recovery_root.clone();
        self.start(Kind::Load, move |cancel| {
            tasks::check(cancel)?;
            let editor = Editor::new(Document::open(&path)?);
            let layout = paginate(&editor, cancel)?;
            tasks::check(cancel)?;
            let (autosave, error) = autosave(root);
            Ok(Outcome::Installed(Box::new(Installed {
                editor,
                layout,
                filename: path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                path: Some(path),
                autosave,
                error,
                restored: false,
                message: String::new(),
            })))
        })
    }
    pub(super) fn restore(&mut self, recovered: Recovered) -> Result<(), String> {
        self.start(Kind::Restore, move |cancel| {
            let layout = paginate(&recovered.editor, cancel)?;
            tasks::check(cancel)?;
            let autosave = Autosave::resume(recovered.session)?;
            Ok(Outcome::Installed(Box::new(Installed {
                editor: recovered.editor,
                layout,
                filename: recovered.name,
                path: None,
                autosave: Some(autosave),
                error: None,
                restored: true,
                message: if recovered.previous_generation {
                    "Восстановлен предыдущий исправный снимок сессии."
                } else {
                    "Несохранённая сессия восстановлена. Сохраните копию DOCX."
                }
                .into(),
            })))
        })
    }
    pub(super) fn scan(&mut self, startup: bool, skip: BTreeSet<PathBuf>) -> Result<(), String> {
        let Some(root) = self.recovery_root.clone() else {
            return Ok(());
        };
        self.start(Kind::Scan, move |cancel| {
            tasks::check(cancel)?;
            let candidate = recovery::find(&root, &skip)?;
            tasks::check(cancel)?;
            Ok(Outcome::Found(Box::new(ScanResult {
                candidate,
                startup,
                skip,
            })))
        })
    }
    pub(super) fn save_to(&mut self, path: PathBuf) -> Result<(), String> {
        let editor = self
            .editor
            .as_ref()
            .ok_or("Сначала откройте документ")?
            .clone();
        let count = self.layout.as_ref().map(|l| l.pages.len());
        self.start(Kind::Save, move |_| {
            editor.document().save_new(&path, count)?;
            Ok(Outcome::Saved(path, editor.checkpoint_stamp()))
        })
    }
    pub(super) fn apply(&mut self, edit: Edit) -> Result<(), String> {
        if self.closing || matches!(self.after, Some(Action::Close)) {
            return Err("Завершение работы: дождитесь текущей операции".into());
        }
        if self.busy() {
            if !matches!(self.kind, Some(Kind::Edit | Kind::Save)) {
                return Err("Дождитесь открытия документа или отмените его клавишей Esc".into());
            }
            let bytes = if let Edit::Insert(s) = &edit {
                s.len()
            } else {
                0
            };
            if self.edits.len() >= 256 || self.edit_bytes.saturating_add(bytes) > 1024 * 1024 {
                return Err(
                    "Очередь ввода заполнена: последняя команда не принята, дождитесь пересчёта"
                        .into(),
                );
            }
            self.edit_bytes += bytes;
            self.edits.push_back(edit);
            return Ok(());
        }
        let Some(mut editor) = self.editor.clone() else {
            return Ok(());
        };
        let base_stamp = editor.checkpoint_stamp();
        self.start(Kind::Edit, move |cancel| {
            tasks::check(cancel)?;
            let layout = if let Some(change) = editor.prepare(edit)? {
                let factory: IDWriteFactory =
                    unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
                        .map_err(|e| e.to_string())?;
                let layout = crate::layout::build_cancelled(&factory, &change.document, cancel)
                    .map_err(|e| e.to_string())?;
                tasks::check(cancel)?;
                editor.commit(change)?;
                Some(layout)
            } else {
                None
            };
            Ok(Outcome::Edited(editor, layout, base_stamp))
        })
    }
    pub(super) fn cancel_read(&mut self) {
        if matches!(self.kind, Some(Kind::Load | Kind::Scan | Kind::Restore)) {
            if let Some(work) = &self.work {
                work.cancel();
            }
            self.message = "Отмена операции…".into();
        }
    }
    pub(super) fn poll_work(&mut self) -> (bool, Option<ScanResult>) {
        let Some(done) = self.work.as_mut().and_then(Background::poll) else {
            return (false, None);
        };
        let kind = self.kind.take();
        if done.cancelled {
            if let Ok(Outcome::Installed(mut value)) = done.result
                && let Some(autosave) = value.autosave.take()
            {
                self.retired.push(autosave.retire());
            }
            self.message = "Операция отменена. Текущий документ сохранён в памяти.".into();
            return (true, None);
        }
        match done.result {
            Err(error) => {
                self.operation_error = Some(error.clone());
                self.after = None;
                if kind == Some(Kind::Edit) {
                    let count = self.edits.len();
                    self.edits.clear();
                    self.edit_bytes = 0;
                    self.message = format!("{error}. Последующих команд не применено: {count}.");
                } else {
                    self.message = error;
                }
            }
            Ok(Outcome::Found(scan)) => {
                self.message.clear();
                return (true, Some(*scan));
            }
            Ok(Outcome::Installed(value)) => {
                self.operation_error = None;
                self.finish_autosave();
                self.zoom = render::fit_zoom(self.width, self.height, value.editor.document());
                self.scroll = 0.0;
                self.confirmed_stamp = value.restored.then(|| value.editor.checkpoint_stamp());
                self.editor = Some(value.editor);
                self.layout = Some(value.layout);
                self.filename = value.filename;
                self.file_path = value.path;
                self.autosave = value.autosave;
                self.autosave_error = value.error;
                self.message = self
                    .autosave_error
                    .as_ref()
                    .map_or(value.message, |e| format!("Автосохранение недоступно: {e}"));
                self.last_attempt = std::time::Instant::now();
                self.upstream = false;
                self.preferred_x = None;
                self.input.clear();
                self.update_autosave_status();
                self.reveal();
            }
            Ok(Outcome::Edited(editor, layout, base_stamp)) => {
                if self
                    .editor
                    .as_ref()
                    .is_none_or(|current| current.checkpoint_stamp() != base_stamp)
                {
                    self.after = None;
                    self.edits.clear();
                    self.edit_bytes = 0;
                    self.message =
                        "Устаревшая вёрстка отклонена. Текущий документ не изменён.".into();
                    self.operation_error = Some(self.message.clone());
                    return (true, None);
                }
                let was_dirty = self.editor.as_ref().is_some_and(Editor::dirty);
                self.editor = Some(editor);
                if let Some(layout) = layout {
                    self.layout = Some(layout);
                }
                let dirty = self.editor.as_ref().is_some_and(Editor::dirty);
                if was_dirty && !dirty {
                    self.clear_autosave();
                }
                if !was_dirty && dirty {
                    self.last_attempt = std::time::Instant::now();
                }
                self.upstream = false;
                self.preferred_x = None;
                self.message.clear();
                self.update_autosave_status();
                self.reveal();
            }
            Ok(Outcome::Saved(path, stamp)) => {
                self.operation_error = None;
                if let Some(editor) = self.editor.as_mut()
                    && editor.checkpoint_stamp() == stamp
                {
                    editor.mark_saved();
                    self.clear_autosave();
                    self.filename = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    self.file_path = Some(path);
                    self.message = "Копия сохранена. Исходный документ не изменён.".into();
                } else {
                    self.message =
                        "Копия сохранена; более новые изменения остаются несохранёнными.".into();
                }
            }
        }
        (true, None)
    }
    pub(super) fn next_edit(&mut self) -> bool {
        if self.busy() {
            return false;
        }
        if let Some(edit) = self.edits.pop_front() {
            if let Edit::Insert(s) = &edit {
                self.edit_bytes -= s.len();
            }
            // Already accepted input must finish before a deferred Close action.
            let after = self.after.take();
            if let Err(error) = self.apply(edit) {
                self.message = error;
            }
            self.after = after;
            true
        } else {
            false
        }
    }
    pub(super) fn poll_retired(&mut self) {
        for index in (0..self.retired.len()).rev() {
            match self.retired[index].try_recv() {
                Ok(Ok(())) => {
                    self.retired.remove(index);
                }
                Ok(Err(error)) => {
                    self.retired.remove(index);
                    self.message = error;
                    self.closing = false;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.retired.remove(index);
                    self.message = "Автосохранение не подтвердило завершение".into();
                    self.closing = false;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_layout_cannot_replace_a_newer_editor_state() {
        let original = Editor::new(
            Document::from_bytes(
                include_bytes!("../../../../tests/fixtures/formatting.docx").to_vec(),
            )
            .unwrap(),
        );
        let base_stamp = original.checkpoint_stamp();
        let mut outdated = original.clone();
        let change = outdated
            .prepare(Edit::Insert("stale".into()))
            .unwrap()
            .unwrap();
        outdated.commit(change).unwrap();
        let mut state = State::empty(96.0);
        state.editor = Some(original);
        state
            .start(Kind::Edit, move |_| {
                Ok(Outcome::Edited(outdated, None, base_stamp))
            })
            .unwrap();
        state.editor.as_mut().unwrap().select_all();
        let expected = state.editor.as_ref().unwrap().selection();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while state.busy() {
            assert!(std::time::Instant::now() < deadline);
            state.poll_work();
            std::thread::yield_now();
        }
        assert_eq!(state.editor.as_ref().unwrap().selection(), expected);
        assert!(!state.editor.as_ref().unwrap().dirty());
        assert!(
            state
                .operation_error
                .as_ref()
                .unwrap()
                .starts_with("Устаревшая вёрстка")
        );
    }
}
