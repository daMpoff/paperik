$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem
$parts = [ordered]@{}
foreach ($file in Get-ChildItem -LiteralPath (Join-Path $PSScriptRoot 'source') -Recurse -File) {
    $name = [IO.Path]::GetRelativePath((Join-Path $PSScriptRoot 'source'), $file.FullName).Replace('\', '/')
    $parts[$name] = [IO.File]::ReadAllBytes($file.FullName)
}
$source = Join-Path $PSScriptRoot 'source-blocks'
$parts['word/document.xml'] = [IO.File]::ReadAllBytes((Join-Path $source 'document.xml'))
$styles = [Text.Encoding]::UTF8.GetString($parts['word/styles.xml'])
$extra = [IO.File]::ReadAllText((Join-Path $source 'table-style.xml'))
$parts['word/styles.xml'] = [Text.Encoding]::UTF8.GetBytes($styles.Replace('</w:styles>', "$extra</w:styles>"))
$rels = [Text.Encoding]::UTF8.GetString($parts['word/_rels/document.xml.rels'])
$image = '<Relationship Id="image" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/sample.png"/>'
$parts['word/_rels/document.xml.rels'] = [Text.Encoding]::UTF8.GetBytes($rels.Replace('</Relationships>', "$image</Relationships>"))
$types = [Text.Encoding]::UTF8.GetString($parts['[Content_Types].xml'])
$parts['[Content_Types].xml'] = [Text.Encoding]::UTF8.GetBytes($types.Replace('</Types>', '<Default Extension="png" ContentType="image/png"/></Types>'))
$parts['word/media/sample.png'] = [Convert]::FromBase64String([IO.File]::ReadAllText((Join-Path $source 'sample.png.base64')))
$stream = [IO.File]::Open((Join-Path $PSScriptRoot 'blocks.docx'), [IO.FileMode]::Create)
$archive = [IO.Compression.ZipArchive]::new($stream, [IO.Compression.ZipArchiveMode]::Create)
try {
    foreach ($name in ($parts.Keys | Sort-Object)) {
        $entry = $archive.CreateEntry($name, [IO.Compression.CompressionLevel]::Optimal)
        $entry.LastWriteTime = [DateTimeOffset]::new(2000, 1, 1, 0, 0, 0, [TimeSpan]::Zero)
        $writer = $entry.Open()
        try { $writer.Write($parts[$name]) } finally { $writer.Dispose() }
    }
} finally { $archive.Dispose(); $stream.Dispose() }
