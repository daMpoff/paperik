[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$ExecutablePath
)

$ErrorActionPreference = 'Stop'
$resolvedExecutable = (Resolve-Path -LiteralPath $ExecutablePath).Path
$startInfo = [System.Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $resolvedExecutable
$startInfo.Arguments = '--smoke-test'
$startInfo.WorkingDirectory = [System.IO.Path]::GetDirectoryName($resolvedExecutable)
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $true
$startInfo.RedirectStandardError = $true
# Affect only this child: the user's PATH and Rust setup remain untouched.
$startInfo.Environment['PATH'] = [Environment]::SystemDirectory
$process = [System.Diagnostics.Process]::Start($startInfo)
try {
    if (-not $process.WaitForExit(10000)) {
        $process.Kill()
        $process.WaitForExit()
        throw 'Проверка автономного запуска превысила 10 секунд.'
    }
    $errorText = $process.StandardError.ReadToEnd()
    if ($process.ExitCode -ne 0) {
        throw "Автономный запуск завершился с кодом $($process.ExitCode): $errorText"
    }
    Write-Output 'Автономный запуск: OK; PATH дочернего процесса содержит только System32.'
}
finally {
    $process.Dispose()
}
