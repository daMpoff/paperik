# Компоненты, включённые в сборку Paperik

Собственная лицензия исходников Paperik пока не выбрана. Уведомления ниже относятся к внешним компонентам и не предоставляют отдельную открытую лицензию на код Paperik.

- `windows-sys 0.61.2` и `windows-link 0.2.1`: copyright Microsoft Corporation, MIT OR Apache-2.0. Для этой поставки выбран MIT; точные тексты находятся в `licenses/windows-sys-MIT.txt` и `licenses/windows-link-MIT.txt`. Исходники: https://github.com/microsoft/windows-rs.
- Дополнительные закреплённые Cargo-компоненты (DEFLATE, XML, Windows COM и транзитивные зависимости): полный список версий/SPDX — `licenses/cargo/README.md`, точные copyright и тексты — в одноимённых каталогах. Для альтернативных MIT/Apache условий выбран MIT; сохранены также обязательные Unicode-3.0 уведомления `unicode-ident`. Все upstream-уведомления включены без сокращения, также для build-time компонентов. Обоснование — `docs/decisions/0004-docx-viewer.md`.
- Стандартная библиотека Rust 1.98.1 и включённые компоненты: copyright The Rust Project Contributors и других указанных правообладателей. Уведомления конкретного toolchain при упаковке копируются в `licenses/rust/COPYRIGHT-library.html` вместе с каталогом текстов лицензий. Исходники: https://github.com/rust-lang/rust/tree/1.98.1.
- MinGW-w64 runtime, включённый статически компонентом `rust-mingw`: уведомления находятся в `licenses/mingw-w64/`. Файл `COPYING.MinGW-w64-runtime_COPYING.MinGW-w64-runtime.txt` специально предназначен upstream для распространения с собранными программами. Ревизия источника текстов указана в `SOURCE-REVISION.txt`; она не является заявлением о ревизии бинарных библиотек Rust. Исходники: https://github.com/mingw-w64/mingw-w64.
- LLVM libunwind, включённый статически: Apache-2.0 WITH LLVM-exception, с дополнительными уведомлениями из upstream. Текст: `licenses/libunwind-LICENSE.txt`, источник — https://github.com/llvm/llvm-project/blob/llvmorg-22.1.8/libunwind/LICENSE.TXT. Версия LLVM соответствует выводу установленного Rust 1.98.1.

В поставку не включаются компилятор, Cargo, линкер, Microsoft Build Tools, системные Windows DLL, файлы шрифтов, Ollama или веса моделей. Windows API и Universal CRT предоставляются операционной системой.

Набор уведомлений для runtime сохранён консервативно, без удаления разделов. Перед публичным выпуском требуется сверить бинарное происхождение runtime и актуальность всех уведомлений для выбранного toolchain. Наличие лицензии в полном наборе уведомлений не означает, что каждый описанный компонент присутствует в конечном exe.
