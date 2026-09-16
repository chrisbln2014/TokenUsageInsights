# TokenUsageInsights 服務重啟（由 TokenUsageInsights-NightlyRestart 以最高權限執行）
# NSSM 2.24 在 AppRotateOnline=1 時停止服務必定卡在 STOP_PENDING，故卡住時只結束本服務的 nssm 主進程
#
# v2（2026-09-15）：原本用「等待上傳排程出現空檔」判斷是否安全，但上傳每 30 分鐘
# 跑一次、每次約需 28 分鐘，幾乎沒有空檔，導致 2026-09-14 夜間重啟等了 50 分鐘後
# 放棄（restart-service.log: "Drive 上傳 50 分鐘內未結束，本次不重啟"，LastTaskResult=1）。
# 改為主動 Disable 上傳排程（比照換版腳本 upgrade.ps1 的做法）：Disable 後只需等待「可能
# 正在執行中」的那一次上傳結束（最長不超過其本身執行時間，非固定空檔），比被動等待可靠；
# finally 一律 Enable，即使中途失敗也不會讓排程被永久關閉。
$ErrorActionPreference = "Stop"
$service = "TokenUsageInsights"
$uploadTask = "TokenUsageInsights Drive Snapshot Upload"
$inst = "C:\Users\1418\AppData\Local\TokenUsageInsights"
$dashExe = Join-Path $inst "token-usage-insights.exe"
$log = Join-Path $inst "logs\restart-service.log"

function Write-Log([string]$message) {
    $line = "[{0}] {1}" -f (Get-Date -Format "yyyy-MM-dd HH:mm:ss"), $message
    Add-Content -Path $log -Value $line -Encoding utf8
    Write-Output $line
}

# Disable 只擋「未來的新排程觸發」，不會中斷正在執行中的那一次；故仍需等它結束
function Wait-CurrentUploadRunToFinish {
    $deadline = (Get-Date).AddMinutes(40)
    $waited = $false
    while ((Get-Date) -lt $deadline) {
        if ((Get-ScheduledTask -TaskName $uploadTask).State -ne "Running") { return $true }
        if (-not $waited) { Write-Log "上傳排程正在執行中，等待其結束"; $waited = $true }
        Start-Sleep -Seconds 20
    }
    return $false
}

$uploadTaskDisabled = $false
try {
    Write-Log "restart requested"

    Disable-ScheduledTask -TaskName $uploadTask | Out-Null
    $uploadTaskDisabled = $true
    Write-Log "upload task disabled"

    if (-not (Wait-CurrentUploadRunToFinish)) {
        Write-Log "上傳排程 40 分鐘內未結束，本次不重啟"
        exit 1
    }

    Stop-Service $service -NoWait -ErrorAction SilentlyContinue
    $deadline = (Get-Date).AddSeconds(30)
    while ((Get-Service $service).Status -ne "Stopped" -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 500 }

    $cim = Get-CimInstance Win32_Service -Filter "Name='$service'"
    if ($cim.State -ne "Stopped") {
        $hostPid = [int]$cim.ProcessId
        $proc = Get-Process -Id $hostPid -ErrorAction SilentlyContinue
        if (-not ($proc -and $proc.ProcessName -eq "nssm" -and $cim.PathName -match "nssm")) {
            Write-Log "服務卡在 $($cim.State)，但主進程 pid=$hostPid 不是本服務的 nssm，中止"
            exit 1
        }
        Write-Log "stop stuck ($($cim.State)); killing nssm host pid=$hostPid"
        Stop-Process -Id $hostPid -Force
        $deadline = (Get-Date).AddSeconds(30)
        while ((Get-Service $service).Status -ne "Stopped" -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 500 }
    }
    Get-Process token-usage-insights -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $dashExe } | ForEach-Object {
        Write-Log "killing leftover dashboard pid=$($_.Id)"; Stop-Process -Id $_.Id -Force
    }
    if ((Get-Service $service).Status -ne "Stopped") { Write-Log "服務無法停止"; exit 1 }
    Write-Log "service stopped"

    Start-Sleep -Seconds 2
    Start-Service $service
    $deadline = (Get-Date).AddSeconds(120)
    while ((Get-Date) -lt $deadline) {
        try {
            $r = Invoke-RestMethod "http://127.0.0.1:3003/api/claude/dates" -TimeoutSec 10
            if ($null -ne $r.dates) { Write-Log "dashboard up (claude dates=$($r.dates.Count))"; exit 0 }
        } catch { }
        Start-Sleep -Seconds 2
    }
    Write-Log "服務已啟動但 120 秒內 API 無回應 (status=$((Get-Service $service).Status))"
    exit 1
} catch {
    Write-Log "ERROR: $_"
    exit 1
} finally {
    if ($uploadTaskDisabled) {
        Enable-ScheduledTask -TaskName $uploadTask | Out-Null
        Write-Log "upload task re-enabled"
    }
}
