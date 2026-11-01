Paperik 0.1.0 — техническая сборка для Windows x64

Распакуйте архив целиком и запустите paperik.exe.
Это первая сборка с пустым системным окном. Редактирование документов,
чтение DOCX и ИИ ещё не реализованы.

Rust, Cargo, Microsoft Build Tools и Ollama для запуска не требуются.
Используются системные компоненты Windows. Прототип проверен только
на компьютере разработки; поддержка чистой Windows VM ещё не подтверждена.

В PowerShell можно выполнить:
  .\paperik.exe --smoke-test
  .\paperik.exe --version

Лицензионные уведомления внешних компонентов:
  THIRD_PARTY_NOTICES.md
  licenses\

Проект: https://github.com/daMpoff/paperik
