use std::{marker::PhantomData, rc::Rc};
use windows::{
    Data::Json::{IJsonObjectStatics, IJsonValueStatics, JsonObject, JsonValue, JsonValueType},
    Win32::System::WinRT::{
        RO_INIT_MULTITHREADED, RoGetActivationFactory, RoInitialize, RoUninitialize,
    },
    core::{HSTRING, Interface},
};

struct Apartment(PhantomData<Rc<()>>);
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            RoUninitialize();
        }
    }
}
// Factories are owned by this apartment and released BEFORE it is uninitialized.
// Avoid process-wide static activation caches across worker/apartment restarts.
pub(super) struct Json {
    object: IJsonObjectStatics,
    value: IJsonValueStatics,
    _apartment: Apartment,
}
impl Json {
    pub fn new() -> Result<Self, String> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
            .map_err(|_| "Не удалось запустить системный JSON Windows")?;
        let apartment = Apartment(PhantomData);
        let (object, value) = unsafe {
            (
                RoGetActivationFactory(&HSTRING::from("Windows.Data.Json.JsonObject")),
                RoGetActivationFactory(&HSTRING::from("Windows.Data.Json.JsonValue")),
            )
        };
        Ok(Self {
            object: object.map_err(|_| "Системный JSON Windows недоступен")?,
            value: value.map_err(|_| "Системный JSON Windows недоступен")?,
            _apartment: apartment,
        })
    }
    pub fn object(&self, bytes: &[u8]) -> Result<JsonObject, String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "Ollama вернула некорректный UTF-8")?;
        let text = HSTRING::from(text);
        let mut result = std::ptr::null_mut();
        // SAFETY: live factory, valid HSTRING and out-pointer; transferred COM ownership.
        unsafe {
            (self.object.vtable().Parse)(
                self.object.as_raw(),
                std::mem::transmute_copy(&text),
                &mut result,
            )
            .ok()
            .map_err(|_| "Ollama вернула некорректный JSON")?;
            Ok(JsonObject::from_raw(result))
        }
    }
    pub fn quote(&self, text: &str) -> Result<String, String> {
        let text = HSTRING::from(text);
        let mut result = std::ptr::null_mut();
        // SAFETY: live factory, valid HSTRING and out-pointer; transferred COM ownership.
        let value = unsafe {
            (self.value.vtable().CreateStringValue)(
                self.value.as_raw(),
                std::mem::transmute_copy(&text),
                &mut result,
            )
            .ok()
            .map_err(|_| "Не удалось закодировать сообщение")?;
            JsonValue::from_raw(result)
        };
        value
            .Stringify()
            .map(|v| v.to_string())
            .map_err(|_| "Не удалось закодировать сообщение".into())
    }
}
pub(super) fn string(object: &JsonObject, key: &str) -> Option<String> {
    object
        .GetNamedString(&HSTRING::from(key))
        .ok()
        .map(|v| v.to_string())
}
pub(super) fn remote(object: &JsonObject) -> bool {
    ["remote_model", "remote_host"].iter().any(|key| {
        object
            .GetNamedValue(&HSTRING::from(*key))
            .ok()
            .is_some_and(|value| {
                value.ValueType().ok() != Some(JsonValueType::String)
                    || value.GetString().is_ok_and(|s| !s.is_empty())
            })
    })
}
