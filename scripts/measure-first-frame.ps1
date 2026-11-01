[CmdletBinding()]
param(
    [ValidateRange(1, 30)] [int]$Samples = 10,
    [ValidateSet(96, 192)] [int]$Dpi = 96,
    [switch]$Offline
)
$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$text = [IO.File]::ReadAllText((Join-Path $projectRoot 'rust-toolchain.toml'))
$toolchain = [regex]::Match($text, 'channel\s*=\s*"([^"]+)"').Groups[1].Value
$target = 'x86_64-pc-windows-gnullvm'
if (-not $toolchain.EndsWith("-$target")) { throw 'Требуется GNU/LLVM host toolchain.' }
foreach ($name in @('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER')) {
    if ([Environment]::GetEnvironmentVariable($name)) { throw "Уберите переопределение $name." }
}
$cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
$cargo = if ($cargoCommand) { $cargoCommand.Source } else {
    Join-Path ([Environment]::GetFolderPath('UserProfile')) '.cargo/bin/cargo.exe'
}
$arguments = @('test', '--release', '--locked', '--target', $target, '--bin', 'paperik', '--no-run', '--message-format=json')
if ($Offline) { $arguments += '--offline' }
Push-Location -LiteralPath $projectRoot
try {
    $messages = & $cargo "+$toolchain" @arguments
    if ($LASTEXITCODE -ne 0) { throw 'Не удалось собрать release-пробник.' }
    $artifacts = @($messages | ForEach-Object { $_ | ConvertFrom-Json } | Where-Object {
        $_.reason -eq 'compiler-artifact' -and $_.profile.test -and $_.target.name -eq 'paperik' -and $_.executable
    })
    if ($artifacts.Count -ne 1) { throw 'Ожидался один исполняемый файл тестов.' }
    for ($sample = 1; $sample -le $Samples; $sample++) {
        # Alternate order: the new renderer must not always benefit from being second.
        $modes = if ($sample % 2) { @('default', 'production') } else { @('production', 'default') }
        foreach ($mode in $modes) {
            $info = [Diagnostics.ProcessStartInfo]::new()
            $info.FileName = $artifacts[0].executable
            foreach ($argument in @('window::perf::first_frame_report', '--ignored', '--nocapture', '--test-threads=1')) {
                $info.ArgumentList.Add($argument)
            }
            $info.Environment['PAPERIK_QA_RENDER_MODE'] = $mode
            $info.Environment['PAPERIK_QA_RENDER_DPI'] = $Dpi.ToString()
            $info.UseShellExecute = $false
            $info.CreateNoWindow = $true
            $info.RedirectStandardOutput = $true
            $info.RedirectStandardError = $true
            $process = [Diagnostics.Process]::Start($info)
            try {
                $output = $process.StandardOutput.ReadToEndAsync()
                $errors = $process.StandardError.ReadToEndAsync()
                if (-not $process.WaitForExit(20000)) {
                    $process.Kill()
                    $process.WaitForExit()
                    throw 'Пробник первого кадра превысил 20 секунд.'
                }
                if ($process.ExitCode -ne 0) { throw $errors.GetAwaiter().GetResult() }
                $line = @($output.GetAwaiter().GetResult().Split("`n") | Where-Object { $_.Contains('FIRST_FRAME mode=') })
                if ($line.Count -ne 1) { throw 'Нет однозначного отчёта первого кадра.' }
                $row = $line[0].Substring($line[0].IndexOf('FIRST_FRAME')).Trim()
                Write-Output "sample=$sample $row"
            }
            finally { $process.Dispose() }
        }
    }
}
finally { Pop-Location }
