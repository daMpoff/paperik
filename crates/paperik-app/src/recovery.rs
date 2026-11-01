//! Local append-only recovery generations. An OS lock excludes live sessions.
use paperik_document::editor::Editor;
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions, TryLockError},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};
const MAX_FILE: usize = 64 * 1024 * 1024;
const CLOSED: &[u8] = b"PAPERIK-SESSION-CLOSED-1\ncomplete\n";
static COUNTER: AtomicU64 = AtomicU64::new(0);
pub fn default_root() -> Result<PathBuf, String> {
    let base = std::env::var_os("LOCALAPPDATA")
        .filter(|s| !s.is_empty())
        .ok_or("Не найдена локальная папка данных Windows")?;
    let base = PathBuf::from(base);
    if !base.is_absolute() {
        return Err("Некорректная локальная папка Windows".into());
    }
    Ok(base.join("Paperik").join("Recovery"))
}
fn ordinary(path: &Path, directory: bool) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| {
        !m.file_type().is_symlink() && if directory { m.is_dir() } else { m.is_file() }
    })
}
fn session_name(name: &str) -> bool {
    name.len() <= 100
        && name.strip_prefix("session-").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-')
        })
}
fn sequence(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    let digits = name.strip_prefix("generation-")?.strip_suffix(".pkr")?;
    if digits.len() != 20 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}
pub struct Session {
    path: PathBuf,
    lock: File,
    next: u64,
    last_valid: Option<u64>,
}
enum Loaded {
    Empty,
    Closed,
    Draft(Box<Editor>, String, bool),
}
impl Session {
    pub fn create(root: &Path) -> Result<Self, String> {
        fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for _ in 0..128 {
            let path = root.join(format!(
                "session-{:x}-{stamp:x}-{:x}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self::claim(path)?.ok_or("Новая сессия неожиданно занята".into()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.to_string()),
            }
        }
        Err("Не удалось выделить папку автосохранения".into())
    }
    fn claim(path: PathBuf) -> Result<Option<Self>, String> {
        if !ordinary(&path, true) {
            return Ok(None);
        }
        let lock_path = path.join("lock");
        match fs::symlink_metadata(&lock_path) {
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                return Ok(None);
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .map_err(|e| e.to_string())?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(e)) => return Err(e.to_string()),
        }
        let mut result = Self {
            path,
            lock,
            next: 1,
            last_valid: None,
        };
        if let Some((seq, _)) = result.records()?.first() {
            result.next = seq.checked_add(1).ok_or("Переполнен счётчик сессии")?;
        }
        Ok(Some(result))
    }
    fn records(&self) -> Result<Vec<(u64, PathBuf)>, String> {
        let mut files = Vec::new();
        for (index, entry) in fs::read_dir(&self.path)
            .map_err(|e| e.to_string())?
            .take(1025)
            .enumerate()
        {
            if index == 1024 {
                return Err("В папке сессии слишком много файлов".into());
            }
            let path = entry.map_err(|e| e.to_string())?.path();
            if let Some(number) = sequence(&path)
                && ordinary(&path, false)
            {
                files.push((number, path));
            }
            if files.len() > 256 {
                return Err("В папке сессии слишком много поколений".into());
            }
        }
        files.sort_unstable_by_key(|a| std::cmp::Reverse(a.0));
        Ok(files)
    }
    fn latest(&mut self) -> Result<Loaded, String> {
        let files = self.records()?;
        let mut bad = false;
        for (index, (seq, path)) in files.iter().enumerate() {
            let bytes = match read_bounded(path) {
                Ok(bytes) => bytes,
                Err(_) => {
                    bad = true;
                    continue;
                }
            };
            if bytes == CLOSED {
                self.last_valid = Some(*seq);
                return Ok(Loaded::Closed);
            }
            match Editor::from_checkpoint(&bytes) {
                Ok((editor, name)) if editor.dirty() => {
                    self.last_valid = Some(*seq);
                    return Ok(Loaded::Draft(Box::new(editor), name, index != 0));
                }
                _ => bad = true,
            }
        }
        if bad {
            Err("В сессии нет исправного снимка; её файлы оставлены для проверки".into())
        } else {
            Ok(Loaded::Empty)
        }
    }
    fn append(&mut self, bytes: &[u8], clean: bool) -> Result<(), String> {
        if bytes.len() > MAX_FILE {
            return Err("Снимок автосохранения превышает 64 МиБ".into());
        }
        let number = self.next;
        self.next = self
            .next
            .checked_add(1)
            .ok_or("Переполнен счётчик сессии")?;
        let path = self.path.join(format!("generation-{number:020}.pkr"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        let write = (|| -> std::io::Result<()> {
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            let mut check = File::open(&path)?;
            if check.metadata()?.len() != bytes.len() as u64 {
                return Err(std::io::Error::other("Размер снимка не совпал"));
            }
            let mut buffer = [0u8; 65536];
            for chunk in bytes.chunks(buffer.len()) {
                check.read_exact(&mut buffer[..chunk.len()])?;
                if &buffer[..chunk.len()] != chunk {
                    return Err(std::io::Error::other(
                        "Проверка записанного снимка не пройдена",
                    ));
                }
            }
            Ok(())
        })();
        if let Err(error) = write {
            let _ = fs::remove_file(&path);
            return Err(error.to_string());
        }
        let previous = self.last_valid.replace(number);
        // Never delete the previous valid draft before a complete new generation exists.
        for (seq, path) in self.records()? {
            if seq != number && (clean || Some(seq) != previous) {
                fs::remove_file(path).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
    fn checkpoint(&mut self, editor: &Editor, name: &str) -> Result<(), String> {
        if !editor.dirty() {
            return self.clean();
        }
        self.append(&editor.checkpoint(name)?, false)
    }
    fn clean(&mut self) -> Result<(), String> {
        self.append(CLOSED, true)
    }
    fn remove_closed(self) {
        // Only recognized files in this exclusively locked session are eligible.
        if let Ok(files) = self.records() {
            for (_, path) in files {
                let _ = fs::remove_file(path);
            }
        }
        let Self { path, lock, .. } = self;
        drop(lock);
        let _ = fs::remove_file(path.join("lock"));
        let _ = fs::remove_dir(path);
    }
}
fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_FILE as u64 {
        return Err("Снимок слишком велик".into());
    }
    let mut bytes = Vec::new();
    file.take((MAX_FILE + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_FILE {
        return Err("Снимок слишком велик".into());
    }
    Ok(bytes)
}
pub struct Recovered {
    pub editor: Editor,
    pub name: String,
    pub previous_generation: bool,
    pub session: Session,
}
pub fn find(root: &Path, skip: &BTreeSet<PathBuf>) -> Result<Option<Recovered>, String> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let mut folders = Vec::new();
    for (index, entry) in entries.take(513).enumerate() {
        if index == 512 {
            return Err("Слишком много локальных сессий восстановления".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if ordinary(&path, true)
            && path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(session_name)
            && !skip.contains(&path)
        {
            let time = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(UNIX_EPOCH);
            folders.push((time, path));
        }
    }
    if folders.len() > 512 {
        return Err("Слишком много локальных сессий восстановления".into());
    }
    folders.sort_unstable_by_key(|a| std::cmp::Reverse(a.0));
    let mut damaged = false;
    for (_, path) in folders {
        let Some(mut session) = Session::claim(path)? else {
            continue;
        };
        match session.latest() {
            Ok(Loaded::Draft(editor, name, previous_generation)) => {
                return Ok(Some(Recovered {
                    editor: *editor,
                    name,
                    previous_generation,
                    session,
                }));
            }
            Ok(Loaded::Empty | Loaded::Closed) => session.remove_closed(),
            Err(_) => damaged = true,
        }
    }
    if damaged {
        Err("Есть повреждённые черновики без исправного снимка. Они не удалены.".into())
    } else {
        Ok(None)
    }
}
impl Recovered {
    pub fn path(&self) -> &Path {
        &self.session.path
    }
}

enum Job {
    Checkpoint(Box<Editor>, String),
    Clean(mpsc::Sender<Result<(), String>>),
    Stop(mpsc::Sender<Result<(), String>>),
}
pub struct Event {
    pub stamp: u64,
    pub result: Result<(), String>,
}
pub struct Autosave {
    sender: Option<Sender<Job>>,
    events: Receiver<Event>,
    worker: Option<JoinHandle<()>>,
    pub busy: bool,
    disconnected: bool,
    clean_ack: Option<Receiver<Result<(), String>>>,
}
impl Autosave {
    pub fn new(root: &Path) -> Result<Self, String> {
        Self::resume(Session::create(root)?)
    }
    pub fn resume(mut session: Session) -> Result<Self, String> {
        let (sender, queue) = mpsc::channel();
        let (report, events) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("paperik-autosave".into())
            .spawn(move || {
                while let Ok(job) = queue.recv() {
                    match job {
                        Job::Checkpoint(editor, name) => {
                            let stamp = editor.checkpoint_stamp();
                            let result = session.checkpoint(&editor, &name);
                            let _ = report.send(Event { stamp, result });
                        }
                        Job::Clean(ack) => {
                            let _ = ack.send(session.clean());
                        }
                        Job::Stop(ack) => {
                            let result = session.clean();
                            if result.is_ok() {
                                session.remove_closed();
                            }
                            let _ = ack.send(result);
                            return;
                        }
                    }
                }
                // Disconnect/drop is NOT a clean close. Keep the last recovery generation.
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            sender: Some(sender),
            events,
            worker: Some(worker),
            busy: false,
            disconnected: false,
            clean_ack: None,
        })
    }
    pub fn queue(&mut self, editor: &Editor, name: &str) -> Result<bool, String> {
        if self.busy || self.clean_ack.is_some() {
            return Ok(false);
        }
        match self
            .sender
            .as_ref()
            .ok_or("Автосохранение остановлено")?
            .send(Job::Checkpoint(Box::new(editor.clone()), name.into()))
        {
            Ok(()) => {
                self.busy = true;
                Ok(true)
            }
            Err(_) => Err("Фоновое автосохранение остановилось".into()),
        }
    }
    pub fn poll(&mut self) -> Vec<Event> {
        let mut result = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(event) => {
                    self.busy = false;
                    result.push(event);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if !self.disconnected {
                        self.disconnected = true;
                        self.busy = false;
                        result.push(Event {
                            stamp: 0,
                            result: Err("Фоновое автосохранение остановилось".into()),
                        });
                    }
                    break;
                }
            }
        }
        if let Some(ack) = &self.clean_ack {
            match ack.try_recv() {
                Ok(answer) => {
                    self.clean_ack = None;
                    if let Err(error) = answer {
                        result.push(Event {
                            stamp: 0,
                            result: Err(error),
                        });
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.clean_ack = None;
                    result.push(Event {
                        stamp: 0,
                        result: Err("Автосохранение не подтвердило завершение".into()),
                    });
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        result
    }
    pub fn clean_async(&mut self) -> Result<(), String> {
        if self.clean_ack.is_some() {
            return Ok(());
        }
        let (ack, result) = mpsc::channel();
        self.sender
            .as_ref()
            .ok_or("Автосохранение остановлено")?
            .send(Job::Clean(ack))
            .map_err(|_| "Автосохранение остановлено")?;
        self.clean_ack = Some(result);
        Ok(())
    }
    pub fn abandon(mut self) {
        self.sender.take();
        self.worker.take();
    }
    pub fn retire(mut self) -> Receiver<Result<(), String>> {
        let (ack, result) = mpsc::channel();
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(Job::Stop(ack));
        }
        self.worker.take(); // The acknowledgement is polled; UI never joins this thread.
        result
    }
    #[cfg(test)]
    pub fn clean(&mut self) -> Result<(), String> {
        let (ack, result) = mpsc::channel();
        self.sender
            .as_ref()
            .ok_or("Автосохранение остановлено")?
            .send(Job::Clean(ack))
            .map_err(|_| "Фоновое автосохранение остановилось")?;
        result
            .recv_timeout(std::time::Duration::from_secs(30))
            .map_err(|_| "Автосохранение не подтвердило завершение записи".to_owned())?
    }
    #[cfg(test)]
    pub fn finish(mut self) -> Result<(), String> {
        let (ack, result) = mpsc::channel();
        self.sender
            .as_ref()
            .ok_or("Автосохранение остановлено")?
            .send(Job::Stop(ack))
            .map_err(|_| "Фоновое автосохранение остановилось")?;
        let result = match result.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(result) => result,
            Err(_) => {
                self.worker.take();
                return Err("Не удалось завершить автосохранение".into());
            }
        };
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| "Ошибка потока автосохранения".to_owned())?;
        }
        result
    }
}
impl Drop for Autosave {
    fn drop(&mut self) {
        // End the worker without a clean marker, retaining recovery data.
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests;
