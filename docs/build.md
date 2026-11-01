# Сборка Paperik для Windows

## Выбранная среда

Rust 1.98.1, host и target `x86_64-pc-windows-gnullvm`. Используются `rust-lld`, `rust-mingw` и статическая линковка runtime. Microsoft Build Tools, MSVC и Windows SDK для этой чистой Rust-сборки не требуются. Go и отдельный LLVM-MinGW не устанавливаются.

Выбранная GNU/LLVM-цель имеет уровень поддержки Rust Tier 2; это учитывается при испытаниях и обновлении toolchain. Добавление C/C++-зависимости требует пересмотра инструкции.

## Подготовка

Установить rustup из официального источника https://rustup.rs/ и выполнить в PowerShell:

```powershell
rustup toolchain install 1.98.1-x86_64-pc-windows-gnullvm --profile minimal --component rustfmt --component clippy --component rust-mingw
```

`rust-toolchain.toml` закрепляет ту же версию для проекта. Основной toolchain пользователя не меняется. `.cargo/config.toml` выбирает GNU/LLVM, линкер `rust-lld` и статическое включение CRT/runtime. Не заменять линкер на `link.exe`.

## Команды из корня репозитория

```powershell
pwsh -NoProfile -File scripts/build.ps1 -Configuration Debug
pwsh -NoProfile -File scripts/build.ps1 -Configuration Release -Check -Package
```

Требуется PowerShell 7 для скриптов проверки автономного запуска. Обычная сборка также доступна через `cargo build --locked` и `cargo build --locked --release`. После первой загрузки Cargo-зависимостей скрипт поддерживает `-Offline`.

Проверка `-Check` выполняет форматирование, Clippy со всеми целями и запретом предупреждений, затем интеграционные тесты. Поставка создаётся только после успешного запуска с PATH, содержащим только System32.

Эти же команды выполняет [Windows CI](ci.md) на push в `main`, pull request и ручном запуске. Он дополнительно проверяет, что намеренная ошибка форматирования останавливает сборку, и открывает собственную DOCX-фикстуру из portable-поставки.

Исполняемый файл находится в `target/x86_64-pc-windows-gnullvm/release/paperik.exe`. Архив с уведомлениями — в `dist/paperik-0.5.0-windows-x64.zip`, рядом SHA-256. Для запуска следует распаковать весь архив и открыть exe; Rust, Cargo и Ollama для этого прототипа не требуются.

## Что умеет эта сборка

Открывает DOCX и позволяет редактировать текст в памяти, выделять, копировать, отменять и повторять правки. Сохранение доступно через «Сохранить копию» / Ctrl+S, без замены существующего файла; см. [редактор](editor.md). Поддержка и клавиши описаны в [docx-viewer.md](docx-viewer.md). `--inspect файл.docx` возвращает структурные счётчики без содержимого текста. `--smoke-test [файл.docx]` создаёт скрытое окно, меняет размер, рассчитывает и рисует страницы, затем закрывается. Неизвестные аргументы и повреждённый документ возвращают ненулевой код; скрытая проверка не показывает модальных ошибок.

Проверить поставку с документом:

```powershell
pwsh -NoProfile -File scripts/test-standalone.ps1 -ExecutablePath dist/paperik-0.5.0-windows-x64/paperik.exe -DocumentPath tests/fixtures/formatting.docx
```

При обычном запуске черновики хранятся в `%LOCALAPPDATA%\Paperik\Recovery`, отдельно от исходников и portable-папки. `--smoke-test` и диагностика не сканируют пользовательские сессии. [Автосохранение и восстановление](recovery.md).

## Границы проверки

Локальные debug/release-сборки и запуск вне среды Cargo проверены на текущем компьютере. Очистка PATH проверяет отсутствие зависимости от установленного Rust в этой конфигурации, но не заменяет проверку чистой Windows VM и всех поддерживаемых версий ОС. PPK-001 остаётся открытой до такого испытания. Полноценная поставка редактора PPK-016 также остаётся отдельной задачей.
