# Windows CI

[Workflow](../.github/workflows/windows.yml) запускается на push в `main`, pull request и вручную через workflow_dispatch. Статус и журналы — в [GitHub Actions](https://github.com/daMpoff/paperik/actions/workflows/windows.yml).

## Что проверяется

Используется GitHub-hosted `windows-2025`. Rust устанавливается по `rust-toolchain.toml` с GNU/LLVM host, rustfmt, Clippy и rust-mingw. Host проверяется через `rustc -vV`; target, rust-lld и статическая линковка берутся из `.cargo/config.toml`. MSVC и Microsoft Build Tools не вызываются. Отдельный action установки Rust и кэш сборки не используются.

1. [Проверка остановки сборки](../scripts/test-ci-guard.ps1) временно добавляет ошибку форматирования в собственный исходник и вызывает `build.ps1 -Check -Offline`. Проверка требует отказа именно на форматировании, восстанавливает исходные байты в `finally` и повторяет rustfmt на восстановленном файле. Она не меняет пользовательские документы.
2. `build.ps1 -Configuration Debug` собирает debug.
3. `build.ps1 -Configuration Release -Check -Package` выполняет rustfmt, Clippy с `-D warnings`, unit-/интеграционные тесты, release-сборку, упаковку уведомлений и скрытый автономный запуск с PATH только System32.
4. Дополнительный скрытый запуск поставки открывает собственную фикстуру `tests/fixtures/formatting.docx`.
5. Только после успешных проверок загружаются ZIP и SHA-256 из `dist/`.

Сборка останавливается при ненулевом коде или исключении. Ошибка проверяемого исходного кода не маскируется через continue-on-error. Намеренный отказ внутри проверки форматирования считается успехом только после восстановления файла и успешного повторного rustfmt.

## Доступ и артефакты

Workflow имеет `contents: read`, не использует секреты и не сохраняет Git-учётные данные checkout. Официальные checkout и upload-artifact закреплены полными SHA; версии указаны рядом для обновления. Применяются события pull_request, а не pull_request_target.

Артефакт называется `paperik-windows-x64-<commit SHA>` и хранится 7 дней. В него входят только ZIP поставки и файл контрольной суммы. Исходники, тестовые документы и черновики не загружаются отдельными артефактами. Этот workflow не создаёт тегов, коммитов и релизов и не изменяет Latest.

## Подтверждённый запуск

1 ноября 2026 года [запуск 41342802097](https://github.com/daMpoff/paperik/actions/runs/41342802097) для коммита `51f31f548fb6792e795f09743257a390f6e42dc4` завершился успешно. Подтверждены GNU/LLVM host, остановка на намеренной ошибке форматирования, восстановление исходника, debug/release, Clippy и 59 тестов (19 приложения, 7 интеграционных, 33 ядра; отдельный crash-helper вызывается родительским тестом). Оба автономных запуска прошли; собственная DOCX-фикстура отобразилась на двух страницах. ZIP и SHA-256 загружены в артефакт этого прогона. PPK-002 закрыта по реальному результату Actions.

Скачанный артефакт проверен по digest Actions и внутреннему SHA-256 поставки. В ZIP 103 файла: exe, README и лицензионные уведомления. DOCX, PDF, изображения, черновики и DLL в нём отсутствуют.

## Границы приёмки

GitHub-hosted Windows Server не заменяет испытание поддерживаемой пользовательской Windows на чистой VM, ручной прогон интерфейса/IME и независимую проверку экспорта. PPK-001 и PPK-016 остаются открытыми до своих критериев приёмки.

Правила формата workflow: [GitHub Actions](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax). Действия: [checkout](https://github.com/actions/checkout), [upload-artifact](https://github.com/actions/upload-artifact). Условия выбранной Rust-сборки — [build.md](build.md).
