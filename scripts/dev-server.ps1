param([switch]$CreateAdmin, [string]$Username = 'admin')
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
if (!(Test-Path -LiteralPath '.cache/database-url')) { throw '请先在另一个终端运行 npm run dev:db' }
$env:DATABASE_URL = (Get-Content -LiteralPath '.cache/database-url' -Raw).Trim()
$env:NEWPHOTO_COOKIE_SECURE = 'false'
$env:NEWPHOTO_PUBLIC_ORIGIN = 'http://127.0.0.1:8080'
$env:NEWPHOTO_ALLOWED_UPSTREAMS = '127.0.0.1:19091,127.0.0.1:19092'
if (!(Test-Path -LiteralPath 'secrets/master.key')) { cargo run --locked -p newphoto -- key generate }
if ($CreateAdmin) { cargo run --locked -p newphoto -- admin create --username $Username }
else { cargo run --locked -p newphoto -- serve }
