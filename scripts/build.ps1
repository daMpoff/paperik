[CmdletBinding()]
param(
    [ValidateSet('Debug', 'Release')]
    [string]$Configuration = 'Release',
    [switch]$Check,
    [switch]$Package,
    [switch]$Offline
)

$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$toolchainText = [System.IO.File]::ReadAllText((Join-Path $projectRoot 'rust-toolchain.toml'))
$toolchain = [regex]::Match($toolchainText, 'channel\s*=\s*"([^"]+)"').Groups[1].Value
$target = 'x86_64-pc-windows-gnullvm'
if (-not $toolchain.EndsWith("-$target")) { throw 'Требуется GNU/LLVM host toolchain.' }
foreach ($overrideName in @('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER')) {
    if ([Environment]::GetEnvironmentVariable($overrideName)) {
        throw "Для воспроизводимой сборки уберите переопределение $overrideName из окружения этого процесса."
    }
}
$cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
if ($cargoCommand) {
    $cargo = $cargoCommand.Source
} else {
    $cargo = Join-Path ([Environment]::GetFolderPath('UserProfile')) '.cargo\bin\cargo.exe'
}
if (-not (Test-Path -LiteralPath $cargo)) { throw 'Cargo не найден. См. docs/build.md.' }
$offlineArguments = @()
if ($Offline) { $offlineArguments += '--offline' }

Push-Location -LiteralPath $projectRoot
try {
    if ($Check) {
        & $cargo "+$toolchain" fmt --all -- --check
        if ($LASTEXITCODE -ne 0) { throw 'Проверка форматирования не пройдена.' }
        & $cargo "+$toolchain" clippy --locked --all-targets --target $target @offlineArguments -- -D warnings
        if ($LASTEXITCODE -ne 0) { throw 'Проверка Clippy не пройдена.' }
        & $cargo "+$toolchain" test --locked --target $target @offlineArguments
        if ($LASTEXITCODE -ne 0) { throw 'Тесты не пройдены.' }
    }
    $buildArguments = @('build', '--locked', '--target', $target) + $offlineArguments
    $profileDirectory = 'debug'
    if ($Configuration -eq 'Release') {
        $buildArguments += '--release'
        $profileDirectory = 'release'
    }
    & $cargo "+$toolchain" @buildArguments
    if ($LASTEXITCODE -ne 0) { throw 'Сборка не выполнена.' }
    $metadataText = & $cargo "+$toolchain" metadata --no-deps --format-version 1 --locked @offlineArguments
    if ($LASTEXITCODE -ne 0) { throw 'Не получены метаданные workspace.' }
    $metadata = $metadataText | ConvertFrom-Json
    $binary = Join-Path $metadata.target_directory "$target\$profileDirectory\paperik.exe"
    if (-not $Package) {
        Write-Output "Собрано: $binary"
        return
    }
    if ($Configuration -ne 'Release') { throw 'Поставка создаётся только для Release.' }
    $version = ($metadata.packages | Where-Object name -eq 'paperik-app').version
    $packageName = "paperik-$version-windows-x64"
    $distRoot = Join-Path $projectRoot 'dist'
    $packageRoot = Join-Path $distRoot $packageName
    New-Item -ItemType Directory -Path $packageRoot -Force | Out-Null
    # A previous manual DLL copy must not hide a missing runtime dependency.
    if (Get-ChildItem -LiteralPath $packageRoot -File -Filter '*.dll') {
        throw 'В папке поставки обнаружены DLL. Используйте отдельную чистую папку поставки.'
    }
    Copy-Item -LiteralPath $binary -Destination (Join-Path $packageRoot 'paperik.exe') -Force
    Copy-Item -LiteralPath (Join-Path $projectRoot 'THIRD_PARTY_NOTICES.md') -Destination $packageRoot -Force
    Copy-Item -LiteralPath (Join-Path $projectRoot 'docs\prototype-readme.txt') -Destination (Join-Path $packageRoot 'README.txt') -Force
    Copy-Item -LiteralPath (Join-Path $projectRoot 'licenses') -Destination $packageRoot -Recurse -Force

    $rustup = Join-Path ([System.IO.Path]::GetDirectoryName($cargo)) 'rustup.exe'
    if (-not (Test-Path -LiteralPath $rustup)) { throw 'Для упаковки уведомлений требуется rustup рядом с cargo.' }
    $sysroot = & $rustup run $toolchain rustc --print sysroot
    if ($LASTEXITCODE -ne 0) { throw 'Не найден выбранный sysroot.' }
    $rustNotices = Join-Path $packageRoot 'licenses\rust'
    New-Item -ItemType Directory -Path $rustNotices -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $sysroot 'share\doc\rust\COPYRIGHT-library.html') -Destination $rustNotices -Force
    Copy-Item -LiteralPath (Join-Path $sysroot 'share\doc\rust\licenses') -Destination $rustNotices -Recurse -Force

    & (Join-Path $PSScriptRoot 'test-standalone.ps1') -ExecutablePath (Join-Path $packageRoot 'paperik.exe')
    $zipPath = Join-Path $distRoot "$packageName.zip"
    Compress-Archive -LiteralPath $packageRoot -DestinationPath $zipPath -Force
    $zipHash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
    "$zipHash  $packageName.zip" | Set-Content -LiteralPath "$zipPath.sha256" -Encoding ascii
    Write-Output "Поставка: $zipPath"
}
finally {
    Pop-Location
}
