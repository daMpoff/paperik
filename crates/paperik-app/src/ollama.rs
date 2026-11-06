//! Local Ollama chat. The worker owns HTTP/JSON; UI only polls bounded text deltas.
mod http;
mod json;
use self::{
    http::{Response, check},
    json::Json,
};
use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
};
use windows::core::HSTRING;

pub const MAX_INPUT: usize = 32 * 1024;
pub const MAX_CONTEXT: usize = 256 * 1024;
const MAX_ANSWER: usize = 256 * 1024;
const MAX_JSON: usize = 256 * 1024;
#[derive(Clone)]
pub struct Message {
    pub role: &'static str,
    pub text: String,
}
enum Job {
    Models,
    Chat {
        model: String,
        messages: Vec<Message>,
        tokens: u32,
    },
}
#[derive(Default)]
pub struct Update {
    pub delta: String,
    pub models: Option<Vec<String>>,
    pub done: Option<Result<(), String>>,
}
type Shared = Arc<Mutex<Update>>;
pub struct Client {
    sender: Option<SyncSender<(Job, Arc<AtomicBool>)>>,
    shared: Shared,
    active: Option<Arc<AtomicBool>>,
}
impl Client {
    pub fn new() -> Result<Self, String> {
        Self::at(SocketAddr::from((Ipv4Addr::LOCALHOST, 11434)))
    }
    pub(crate) fn at(address: SocketAddr) -> Result<Self, String> {
        if !address.ip().is_loopback() {
            return Err("Разрешена только локальная Ollama".into());
        }
        let (sender, jobs) = mpsc::sync_channel::<(Job, Arc<AtomicBool>)>(1);
        let shared = Shared::default();
        let report = shared.clone();
        std::thread::Builder::new()
            .name("paperik-ollama".into())
            .spawn(move || {
                while let Ok((job, cancel)) = jobs.recv() {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let json = Json::new()?;
                        match job {
                            Job::Models => {
                                let models = list(&json, address, &cancel)?;
                                report.lock().unwrap_or_else(|e| e.into_inner()).models =
                                    Some(models);
                                Ok(())
                            }
                            Job::Chat {
                                model,
                                messages,
                                tokens,
                            } => chat(
                                &json,
                                address,
                                &model,
                                &messages,
                                tokens,
                                &cancel,
                                |delta| {
                                    report
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .delta
                                        .push_str(delta);
                                },
                            ),
                        }
                    }))
                    .unwrap_or_else(|_| Err("Фоновый запрос Ollama завершился с ошибкой".into()));
                    report.lock().unwrap_or_else(|e| e.into_inner()).done = Some(result);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            sender: Some(sender),
            shared,
            active: None,
        })
    }
    fn start(&mut self, job: Job) -> Result<(), String> {
        if self.busy() {
            return Err("Дождитесь завершения или отмены запроса".into());
        }
        *self.shared.lock().unwrap_or_else(|e| e.into_inner()) = Update::default();
        let cancel = Arc::new(AtomicBool::new(false));
        self.sender
            .as_ref()
            .ok_or("Ollama-клиент остановлен")?
            .try_send((job, cancel.clone()))
            .map_err(|_| "Ollama-клиент недоступен")?;
        self.active = Some(cancel);
        Ok(())
    }
    pub fn refresh(&mut self) -> Result<(), String> {
        self.start(Job::Models)
    }
    pub fn send(&mut self, model: String, messages: Vec<Message>) -> Result<(), String> {
        if model.is_empty()
            || model.len() > 512
            || messages.is_empty()
            || messages.len() > 64
            || messages
                .iter()
                .any(|m| !matches!(m.role, "user" | "assistant") || m.text.len() > MAX_ANSWER)
            || messages.iter().map(|m| m.text.len()).sum::<usize>() > MAX_CONTEXT
        {
            return Err("Диалог слишком велик или некорректен. Начните новый чат.".into());
        }
        self.start(Job::Chat {
            model,
            messages,
            tokens: 2048,
        })
    }
    pub fn busy(&self) -> bool {
        self.active.is_some()
    }
    pub fn cancel(&self) {
        if let Some(cancel) = &self.active {
            cancel.store(true, Ordering::Relaxed);
        }
    }
    pub fn poll(&mut self) -> Update {
        let mut update =
            std::mem::take(&mut *self.shared.lock().unwrap_or_else(|e| e.into_inner()));
        if update.done.is_some()
            && let Some(cancel) = self.active.take()
            && cancel.load(Ordering::Relaxed)
        {
            update.done = Some(Err("Запрос отменён".into()));
        }
        update
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.cancel();
        self.sender.take();
    }
}

fn object(json: &Json, bytes: &[u8]) -> Result<windows::Data::Json::JsonObject, String> {
    let mut depth: usize = 0;
    let mut quoted = false;
    let mut escape = false;
    for byte in bytes {
        if quoted {
            if escape {
                escape = false;
            } else if *byte == b'\\' {
                escape = true;
            } else if *byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > 64 {
                        return Err("Слишком глубокий JSON Ollama".into());
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    json.object(bytes)
}
fn error_response(json: &Json, response: &mut Response<'_>) -> String {
    let detail = response
        .all(MAX_JSON)
        .ok()
        .and_then(|b| object(json, &b).ok())
        .and_then(|o| json::string(&o, "error"));
    format!(
        "Ollama: HTTP {}. {}",
        response.status,
        detail
            .map(|s| s.chars().take(300).collect::<String>())
            .unwrap_or_else(|| "Проверьте состояние модели и обновите список.".into())
    )
}
fn local(object: &windows::Data::Json::JsonObject, name: &str) -> bool {
    !json::remote(object) && !name.ends_with(":cloud") && !name.contains("-cloud")
}
fn list(json: &Json, address: SocketAddr, cancel: &AtomicBool) -> Result<Vec<String>, String> {
    let mut response = Response::open(address, "/api/tags", None, cancel)?;
    if response.status != 200 {
        return Err(error_response(json, &mut response));
    }
    let value = object(json, &response.all(2 * 1024 * 1024)?)?;
    let models = value
        .GetNamedArray(&HSTRING::from("models"))
        .map_err(|_| "В ответе Ollama нет списка моделей")?;
    let size = models.Size().map_err(|e| e.to_string())?;
    if size > 512 {
        return Err("Ollama вернула слишком много моделей".into());
    }
    let mut names = Vec::new();
    for index in 0..size {
        let model = models
            .GetAt(index)
            .and_then(|v| v.GetObject())
            .map_err(|_| "Некорректная модель в списке Ollama")?;
        let name = json::string(&model, "name").ok_or("У модели нет имени")?;
        if name.is_empty() || name.len() > 512 || name.chars().any(char::is_control) {
            return Err("Некорректное имя модели Ollama".into());
        }
        if local(&model, &name) {
            names.push(name);
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}
fn request(json: &Json, model: &str, messages: &[Message], tokens: u32) -> Result<String, String> {
    let mut encoded=vec![format!("{{\"role\":\"system\",\"content\":{}}}",json.quote("Ты помощник Paperik. Отвечай по-русски. Этот чат не имеет доступа к документу и не изменяет файлы. Не утверждай, что выполнил правку документа.")?)];
    for message in messages {
        encoded.push(format!(
            "{{\"role\":{},\"content\":{}}}",
            json.quote(message.role)?,
            json.quote(&message.text)?
        ));
    }
    let data = format!(
        "{{\"model\":{},\"messages\":[{}],\"stream\":true,\"think\":false,\"options\":{{\"num_predict\":{tokens}}}}}",
        json.quote(model)?,
        encoded.join(",")
    );
    if data.len() > 1024 * 1024 {
        return Err("Диалог слишком велик. Начните новый чат.".into());
    }
    Ok(data)
}
fn chat(
    json: &Json,
    address: SocketAddr,
    model: &str,
    messages: &[Message],
    tokens: u32,
    cancel: &AtomicBool,
    mut append: impl FnMut(&str),
) -> Result<(), String> {
    check(cancel)?;
    // Inspect metadata before sending any conversation. Cloud aliases must not receive prompts implicitly.
    let show = format!("{{\"model\":{}}}", json.quote(model)?);
    let mut response = Response::open(address, "/api/show", Some(&show), cancel)?;
    if response.status != 200 {
        return Err(error_response(json, &mut response));
    }
    if !local(&object(json, &response.all(2 * 1024 * 1024)?)?, model) {
        return Err(
            "Выбрана облачная модель. В этой версии доступны только локальные модели.".into(),
        );
    }
    let data = request(json, model, messages, tokens)?;
    let mut response = Response::open(address, "/api/chat", Some(&data), cancel)?;
    if response.status != 200 {
        return Err(error_response(json, &mut response));
    }
    let mut line = Vec::new();
    let mut answer_bytes = 0;
    loop {
        let byte = response.byte()?;
        if let Some(byte) = byte
            && byte != b'\n'
        {
            line.push(byte);
            if line.len() > MAX_JSON {
                return Err("Строка ответа Ollama слишком велика".into());
            }
            continue;
        }
        if !line.is_empty() {
            let frame = object(json, &line)?;
            line.clear();
            if let Some(error) = json::string(&frame, "error") {
                return Err(format!(
                    "Ollama: {}",
                    error.chars().take(300).collect::<String>()
                ));
            }
            if json::remote(&frame) {
                return Err("Ollama сообщила об удалённой модели; ответ остановлен".into());
            }
            if let Ok(message) = frame.GetNamedObject(&HSTRING::from("message"))
                && let Some(content) = json::string(&message, "content")
            {
                answer_bytes += content.len();
                if answer_bytes > MAX_ANSWER {
                    return Err("Ответ достиг предела длины. Частичный текст сохранён.".into());
                }
                append(&content);
            }
            if frame
                .GetNamedBoolean(&HSTRING::from("done"))
                .map_err(|_| "В потоке Ollama отсутствует done")?
            {
                if answer_bytes == 0 {
                    return Err("Модель завершила запрос без текстового ответа".into());
                }
                return Ok(());
            }
        }
        if byte.is_none() {
            return Err("Поток Ollama оборвался до завершения. Частичный ответ сохранён.".into());
        }
    }
}

#[cfg(test)]
mod tests;
