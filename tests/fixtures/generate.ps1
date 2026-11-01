$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem
$sourceRoot = Join-Path $PSScriptRoot 'source'
$destination = Join-Path $PSScriptRoot 'formatting.docx'
$stream = [System.IO.File]::Open($destination, [System.IO.FileMode]::Create)
$archive = [System.IO.Compression.ZipArchive]::new($stream, [System.IO.Compression.ZipArchiveMode]::Create)
try {
    Get-ChildItem -LiteralPath $sourceRoot -File -Recurse -Force | Sort-Object FullName | ForEach-Object {
        $name = $_.FullName.Substring($sourceRoot.Length + 1).Replace('\', '/')
        $entry = $archive.CreateEntry($name, [System.IO.Compression.CompressionLevel]::Optimal)
        $entry.LastWriteTime = [DateTimeOffset]::new(2000, 1, 1, 0, 0, 0, [TimeSpan]::Zero)
        $writer = [System.IO.StreamWriter]::new($entry.Open(), [System.Text.UTF8Encoding]::new($false))
        try { $writer.Write([System.IO.File]::ReadAllText($_.FullName)) } finally { $writer.Dispose() }
    }
} finally { $archive.Dispose(); $stream.Dispose() }
