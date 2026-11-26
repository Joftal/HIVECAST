# PowerShell 7. GUI executables need an explicit wait.
function Invoke-Installer([string]$FilePath, [string[]]$Arguments) {
    $info = [Diagnostics.ProcessStartInfo]::new((Resolve-Path -LiteralPath $FilePath).Path)
    $info.UseShellExecute = $false
    $info.WorkingDirectory = (Get-Location).Path
    foreach ($argValue in $Arguments) { $info.ArgumentList.Add($argValue) }
    $child = [Diagnostics.Process]::Start($info)
    try {
        $child.WaitForExit()
        if ($child.ExitCode -ne 0) { throw "$FilePath exited with code $($child.ExitCode)" }
    } finally {
        $child.Dispose()
    }
}
