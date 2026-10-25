# Per-user installation; no administrator privileges are required.
param([string]$Destination = (Join-Path $env:LOCALAPPDATA 'Programs\Ember'))
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $Destination | Out-Null
foreach ($name in @('ember.exe', 'ember-sign-in.exe', 'ember.ico', 'vcruntime140.dll', 'vcruntime140_1.dll')) {
    $source = Join-Path $PSScriptRoot $name
    if (!(Test-Path -LiteralPath $source)) { throw "Missing package file: $name" }
    Copy-Item -LiteralPath $source -Destination (Join-Path $Destination $name) -Force
}
$exe = Join-Path $Destination 'ember.exe'
$key = 'HKCU:\Software\Classes\ember'
New-Item -Path $key -Force | Out-Null
Set-Item -Path $key -Value 'URL:Ember'
New-ItemProperty -Path $key -Name 'URL Protocol' -Value '' -PropertyType String -Force | Out-Null
New-Item -Path "$key\shell\open\command" -Force | Out-Null
Set-Item -Path "$key\shell\open\command" -Value ('"' + $exe + '" "%1"')
$menu = [Environment]::GetFolderPath('Programs')
$shell = New-Object -ComObject WScript.Shell
$link = $shell.CreateShortcut((Join-Path $menu 'Ember.lnk'))
$link.TargetPath = $exe
$link.WorkingDirectory = $Destination
$link.Description = 'Ember game launcher'
$link.IconLocation = (Join-Path $Destination 'ember.ico')
$link.Save()
Write-Output "Installed Ember to $Destination. Launch it from the Start menu."
