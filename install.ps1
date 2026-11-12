# RustOp Viewer on Windows, in one line in PowerShell:
#
#   irm https://github.com/mmdmcy/rustopviewer/releases/latest/download/install.ps1 | iex
#
# It downloads rustopviewer.exe from the newest release, checks it against
# the release's SHA256SUMS, puts it in %LOCALAPPDATA%\rustopviewer (no
# administrator) and on your PATH, and then shows this PC's own screen in
# your browser as a check: mouse, keyboard, and copy and paste through the
# viewer page. Ctrl+C in this window ends it. Run again, it updates
# rustopviewer.exe or leaves it as it is.
#
# To see this PC from another computer, that computer opens an SSH tunnel
# and runs `rustopviewer serve --until-stdin-closes` here (README.md, "Use").
# It uses cmdlets and plain arguments only, so it also runs where PowerShell
# is held to Constrained Language Mode. $env:ROV_BASE points it at another
# copy of a release (a test, a mirror).

& {
  $ProgressPreference = 'SilentlyContinue'
  $base = 'https://github.com/mmdmcy/rustopviewer/releases/latest/download'
  if ($env:ROV_BASE) { $base = $env:ROV_BASE.TrimEnd('/') }
  $file = 'rustopviewer-windows-x86_64.exe'
  $dir = Join-Path $env:LOCALAPPDATA 'rustopviewer'
  $exe = Join-Path $dir 'rustopviewer.exe'
  $new = "$exe.download"
  $sums = "$exe.sha256sums"
  New-Item -ItemType Directory -Force $dir | Out-Null
  Remove-Item "$exe.old" -ErrorAction SilentlyContinue
  try {
    foreach ($get in @(@($file, $new), @('SHA256SUMS', $sums))) {
      try {
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$($get[0])" -OutFile $get[1] -ErrorAction Stop
      } catch {
        Write-Host "rustopviewer install: could not download $base/$($get[0]) ($($_.Exception.Message)); nothing changed."
        return
      }
    }
    $want = @(Get-Content $sums | ForEach-Object { , @("$_".Trim() -split '\s+') } | Where-Object { $_.Count -eq 2 -and $_[1].TrimStart('*') -eq $file -and $_[0] -match '^[0-9a-f]{64}$' } | ForEach-Object { $_[0] })
    if ($want.Count -ne 1) {
      Write-Host "rustopviewer install: SHA256SUMS does not name $file once; nothing changed."
      return
    }
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $new).Hash.ToLower() -ne $want[0]) {
      Write-Host "rustopviewer install: $file does not match its checksum; nothing changed."
      return
    }
    if (Test-Path $exe) { Move-Item -Force $exe "$exe.old" } # a running one can be moved, not replaced
    Move-Item -Force $new $exe
  } finally {
    Remove-Item $new, $sums -ErrorAction SilentlyContinue
  }

  # On your PATH: for this window now, and for new ones through your
  # account's own Path (no administrator, nobody else's PATH).
  if (@($env:Path -split ';') -notcontains $dir) { $env:Path = "$env:Path;$dir" }
  try {
    $user = "$([Environment]::GetEnvironmentVariable('Path', 'User'))"
    if (@($user -split ';') -notcontains $dir) {
      [Environment]::SetEnvironmentVariable('Path', ((@($user -split ';' | Where-Object { $_ }) + $dir) -join ';'), 'User')
      Write-Host "Added $dir to your PATH (new terminals have it)."
    }
  } catch {
    # Constrained Language Mode: the value Windows reads at sign-in.
    $user = "$((Get-ItemProperty -Path 'HKCU:\Environment' -Name Path -ErrorAction SilentlyContinue).Path)"
    if (@($user -split ';') -notcontains $dir) {
      try {
        Set-ItemProperty -Path 'HKCU:\Environment' -Name Path -Value ((@($user -split ';' | Where-Object { $_ }) + $dir) -join ';') -Type ExpandString -ErrorAction Stop
        Write-Host "Added $dir to your PATH; terminals have it after you sign out and in again (this one has it now)."
      } catch {
        Write-Host "Could not add $dir to your PATH ($($_.Exception.Message)); add it in Settings (Edit environment variables for your account)."
      }
    }
  }

  try { $said = "$(& $exe version 2>&1)".Trim() } catch { $said = $_.Exception.Message }
  if ($said -notmatch '^rustopviewer ') {
    Write-Host "rustopviewer install: $exe does not start here ($said); a policy may block programs in your profile."
    return
  }
  Write-Host "Installed $said in $dir."
  Write-Host "Now this PC's own screen opens in your browser as a check: mouse, keyboard, and copy and paste. Ctrl+C here ends it."
  & $exe serve --open
}
