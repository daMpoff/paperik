[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$source = Join-Path $projectRoot 'crates/paperik-app/src/main.rs'
$original = [IO.File]::ReadAllBytes($source)
$expectedFailure = $false

# Exercise the same entry point as CI, and restore the exact source bytes even on failure.
try {
    $text = [Text.Encoding]::UTF8.GetString($original)
    [IO.File]::WriteAllText($source, "$text`nconst _CI_FORMAT_GUARD:u8=1;`n", [Text.UTF8Encoding]::new($false))
    try {
        & (Join-Path $PSScriptRoot 'build.ps1') -Configuration Release -Check -Offline | Out-Null
    }
    catch {
        if ($_.Exception.Message -ne 'Проверка форматирования не пройдена.') { throw }
        $expectedFailure = $true
    }
}
finally {
    [IO.File]::WriteAllBytes($source, $original)
}
if (-not $expectedFailure) { throw 'Намеренная ошибка форматирования не остановила сборку.' }

Push-Location -LiteralPath $projectRoot
try {
    $text = [IO.File]::ReadAllText('rust-toolchain.toml')
    $toolchain = [regex]::Match($text, 'channel\s*=\s*"([^"]+)"').Groups[1].Value
    & cargo "+$toolchain" fmt --all -- --check
    if ($LASTEXITCODE -ne 0) { throw 'Форматирование после восстановления исходника не прошло проверку.' }
}
finally {
    Pop-Location
}
Write-Output 'CI guard: намеренная ошибка остановила сборку; исходный файл восстановлен, форматирование прошло.'
