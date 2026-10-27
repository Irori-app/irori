# Per-user installation; no administrator privileges are required.
param([string]$Destination = (Join-Path $env:LOCALAPPDATA 'Programs\Irori'))
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $Destination | Out-Null
foreach ($name in @('irori.exe', 'irori-sign-in.exe', 'irori.ico', 'vcruntime140.dll', 'vcruntime140_1.dll')) {
    $source = Join-Path $PSScriptRoot $name
    if (!(Test-Path -LiteralPath $source)) { throw "Missing package file: $name" }
    Copy-Item -LiteralPath $source -Destination (Join-Path $Destination $name) -Force
}
$exe = Join-Path $Destination 'irori.exe'
$key = 'HKCU:\Software\Classes\irori'
New-Item -Path $key -Force | Out-Null
Set-Item -Path $key -Value 'URL:Irori'
New-ItemProperty -Path $key -Name 'URL Protocol' -Value '' -PropertyType String -Force | Out-Null
New-Item -Path "$key\shell\open\command" -Force | Out-Null
Set-Item -Path "$key\shell\open\command" -Value ('"' + $exe + '" "%1"')
$menu = [Environment]::GetFolderPath('Programs')
$shell = New-Object -ComObject WScript.Shell
$link = $shell.CreateShortcut((Join-Path $menu 'Irori.lnk'))
$link.TargetPath = $exe
$link.WorkingDirectory = $Destination
$link.Description = 'Irori game launcher'
$link.IconLocation = (Join-Path $Destination 'irori.ico')
$link.Save()
Write-Output "Installed Irori to $Destination. Launch it from the Start menu."
