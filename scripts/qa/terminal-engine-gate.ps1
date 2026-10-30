param(
    [ValidateSet("", "duplicate", "missing", "missing-equals", "malformed", "nan", "infinity", "negative", "candidate", "no-go", "null", "measurement-failure")]
    [string]$RegressionProbe = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
if (Test-Path -LiteralPath "variable:PSNativeCommandUseErrorActionPreference") {
    $PSNativeCommandUseErrorActionPreference = $false
}

$Header = "RSHELL_TERMINAL_ENGINE_GATE version=1"
$Backend = "alacritty-terminal@0.26.0"
$MeasurementCommand = "cargo bench -p rshell-session --bench terminal_engine --locked"
$FieldNames = @(
    "backend",
    "throughput_bytes",
    "throughput_sample_1_mib_s",
    "throughput_sample_2_mib_s",
    "throughput_sample_3_mib_s",
    "throughput_sample_4_mib_s",
    "throughput_sample_5_mib_s",
    "throughput_median_mib_s",
    "frame_120x40_observations",
    "frame_120x40_p95_ms",
    "scrollback_rows",
    "scrollback_sha256",
    "decision"
)
$SampleFields = @(
    "throughput_sample_1_mib_s",
    "throughput_sample_2_mib_s",
    "throughput_sample_3_mib_s",
    "throughput_sample_4_mib_s",
    "throughput_sample_5_mib_s"
)
$InvariantCulture = [System.Globalization.CultureInfo]::InvariantCulture
$DecimalStyles = [System.Globalization.NumberStyles]::AllowDecimalPoint

function New-GateFailure {
    param([Parameter(Mandatory)][string]$Message)
    throw [System.InvalidOperationException]::new($Message)
}

function Read-CanaryFixture {
    param([Parameter(Mandatory)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        New-GateFailure "terminal-engine fixture is missing"
    }
    try {
        $raw = [System.IO.File]::ReadAllText((Resolve-Path -LiteralPath $Path).Path)
        $fixture = $raw | ConvertFrom-Json
    }
    catch {
        New-GateFailure "terminal-engine fixture is malformed"
    }

    $propertyNames = @(
        "version",
        "throughput_bytes",
        "throughput_samples",
        "minimum_mib_per_second",
        "frame_cols",
        "frame_rows",
        "maximum_frame_p95_ms",
        "scrollback_rows",
        "line_format",
        "input_separator",
        "input_trailing_crlf",
        "canonicalization",
        "sha256"
    )
    if (@($fixture.PSObject.Properties).Count -ne $propertyNames.Count) {
        New-GateFailure "terminal-engine fixture schema is not exact"
    }
    foreach ($name in $propertyNames) {
        $escaped = [regex]::Escape($name)
        if ([regex]::Matches($raw, "(?m)^\s*`"$escaped`"\s*:").Count -ne 1) {
            New-GateFailure "terminal-engine fixture schema is not exact"
        }
    }

    $exact =
        $fixture.version -is [long] -and $fixture.version -eq 1 -and
        $fixture.throughput_bytes -is [long] -and $fixture.throughput_bytes -eq 104857600 -and
        $fixture.throughput_samples -is [long] -and $fixture.throughput_samples -eq 5 -and
        $fixture.minimum_mib_per_second -is [double] -and $fixture.minimum_mib_per_second -eq 40.0 -and
        $fixture.frame_cols -is [long] -and $fixture.frame_cols -eq 120 -and
        $fixture.frame_rows -is [long] -and $fixture.frame_rows -eq 40 -and
        $fixture.maximum_frame_p95_ms -is [double] -and $fixture.maximum_frame_p95_ms -eq 16.0 -and
        $fixture.scrollback_rows -is [long] -and $fixture.scrollback_rows -eq 1000 -and
        $fixture.line_format -is [string] -and $fixture.line_format -ceq "scrollback-{index:04}" -and
        $fixture.input_separator -is [string] -and $fixture.input_separator -ceq "CRLF" -and
        $fixture.input_trailing_crlf -is [bool] -and $fixture.input_trailing_crlf -eq $true -and
        $fixture.canonicalization -is [string] -and
        $fixture.canonicalization -ceq "trim ASCII spaces from each rendered row and join rows with LF"
    if (-not $exact) {
        New-GateFailure "terminal-engine fixture values are not exact"
    }
    if ($null -ne $fixture.sha256 -and
        ($fixture.sha256 -isnot [string] -or $fixture.sha256 -cnotmatch "^[0-9a-f]{64}$")) {
        New-GateFailure "terminal-engine fixture digest is malformed"
    }
    return $fixture
}

function Get-ExactFields {
    param([Parameter(Mandatory)][string]$Text)

    $lines = @($Text -split "\r?\n")
    $headers = @($lines | Where-Object { $_.StartsWith("RSHELL_TERMINAL_ENGINE_GATE", [System.StringComparison]::Ordinal) })
    if ($headers.Count -ne 1 -or $headers[0] -cne $Header) {
        New-GateFailure "terminal-engine output header must occur exactly once"
    }

    $values = @{}
    foreach ($field in $FieldNames) {
        $matches = @($lines | Where-Object { $_.StartsWith($field, [System.StringComparison]::Ordinal) })
        if ($matches.Count -ne 1) {
            New-GateFailure "terminal-engine output field occurrence is invalid"
        }
        $prefix = "$field="
        if (-not $matches[0].StartsWith($prefix, [System.StringComparison]::Ordinal)) {
            New-GateFailure "terminal-engine output field is missing an equals sign"
        }
        $value = $matches[0].Substring($prefix.Length)
        if ($value.Length -eq 0 -or $value.Contains("=")) {
            New-GateFailure "terminal-engine output field value is malformed"
        }
        $values[$field] = $value
    }
    return $values
}

function Write-SanitizedMeasurementEvidence {
    param([Parameter(Mandatory)][string]$Text)

    try {
        $values = Get-ExactFields -Text $Text
    }
    catch {
        return $false
    }
    [Console]::Error.WriteLine($Header)
    foreach ($field in $FieldNames) {
        [Console]::Error.WriteLine("$field=$($values[$field])")
    }
    return $true
}

function Convert-InvariantMeasurement {
    param(
        [Parameter(Mandatory)][string]$Value,
        [Parameter(Mandatory)][string]$Field
    )

    if ($Value -cnotmatch "^[0-9]+\.[0-9]{6}$") {
        New-GateFailure "terminal-engine numeric measurement is malformed"
    }
    $number = 0.0
    if (-not [double]::TryParse($Value, $DecimalStyles, $InvariantCulture, [ref]$number) -or
        [double]::IsNaN($number) -or [double]::IsInfinity($number) -or $number -lt 0.0) {
        New-GateFailure "terminal-engine numeric measurement is invalid"
    }
    return $number
}

function Assert-MeasurementOutput {
    param(
        [Parameter(Mandatory)][string]$Text,
        [Parameter(Mandatory)][psobject]$Fixture
    )

    if ($null -eq $Fixture.sha256) {
        New-GateFailure "terminal-engine fixture digest is unrecorded"
    }
    $values = Get-ExactFields -Text $Text
    if ($values.backend -cne $Backend -or
        $values.throughput_bytes -cne "104857600" -or
        $values.frame_120x40_observations -cne "1000" -or
        $values.scrollback_rows -cne "1000") {
        New-GateFailure "terminal-engine output literals do not match the fixture"
    }
    if ($values.scrollback_sha256 -cnotmatch "^[0-9a-f]{64}$" -or
        $values.scrollback_sha256 -cne $Fixture.sha256) {
        New-GateFailure "terminal-engine output digest does not match the fixture"
    }
    if ($values.decision -cne "GO") {
        New-GateFailure "terminal-engine normal mode did not report GO"
    }

    $samples = @()
    foreach ($field in $SampleFields) {
        $samples += Convert-InvariantMeasurement -Value $values[$field] -Field $field
    }
    $median = Convert-InvariantMeasurement -Value $values.throughput_median_mib_s -Field "throughput_median_mib_s"
    $p95 = Convert-InvariantMeasurement -Value $values.frame_120x40_p95_ms -Field "frame_120x40_p95_ms"
    $sorted = [double[]]@($samples | Sort-Object)
    if ($median -ne $sorted[2]) {
        New-GateFailure "terminal-engine output median is not the third sorted sample"
    }
    if ($median -lt [double]$Fixture.minimum_mib_per_second) {
        New-GateFailure "terminal-engine throughput threshold was not met"
    }
    if ($p95 -ge [double]$Fixture.maximum_frame_p95_ms) {
        New-GateFailure "terminal-engine frame threshold was not met"
    }

    return [pscustomobject]@{
        Values = $values
        Samples = $samples
        Median = $median
        P95 = $p95
    }
}

function Assert-DecisionRecord {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][psobject]$Measurement,
        [Parameter(Mandatory)][psobject]$Fixture
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        New-GateFailure "terminal-engine decision record is missing"
    }
    try {
        $record = [System.IO.File]::ReadAllText((Resolve-Path -LiteralPath $Path).Path)
    }
    catch {
        New-GateFailure "terminal-engine decision record could not be read"
    }
    if (-not $record.Contains("Command: ``$MeasurementCommand``") -or
        -not $record.Contains("Selected sole adapter: ``$Backend``") -or
        -not [regex]::IsMatch($record, "(?m)^- Measured implementation commit: [0-9a-f]{40}\r?$") -or
        -not [regex]::IsMatch($record, "(?m)^- Platform and toolchain: (?!unrecorded\r?$).+\r?$") -or
        -not [regex]::IsMatch($record, "(?m)^Decision: \*\*GO\*\*\r?$")) {
        New-GateFailure "terminal-engine decision record identity is incomplete"
    }
    $recordSamples = @()
    for ($index = 0; $index -lt $SampleFields.Count; $index++) {
        $match = [regex]::Match(
            $record,
            "(?m)^- Throughput sample $($index + 1): ([0-9]+\.[0-9]{6}) MiB/s\r?$"
        )
        if (-not $match.Success) {
            New-GateFailure "terminal-engine decision record sample is missing"
        }
        $recordSamples += Convert-InvariantMeasurement -Value $match.Groups[1].Value -Field "recorded throughput sample"
    }
    $medianMatch = [regex]::Match($record, "(?m)^- Throughput median: ([0-9]+\.[0-9]{6}) MiB/s\r?$")
    $p95Match = [regex]::Match($record, "(?m)^- 120x40 frame p95: ([0-9]+\.[0-9]{6}) ms\r?$")
    if (-not $medianMatch.Success -or -not $p95Match.Success -or
        -not $record.Contains("- Scrollback digest: ``sha256: $($Fixture.sha256)``")) {
        New-GateFailure "terminal-engine decision record measurements are incomplete"
    }
    $recordMedian = Convert-InvariantMeasurement -Value $medianMatch.Groups[1].Value -Field "recorded throughput median"
    $recordP95 = Convert-InvariantMeasurement -Value $p95Match.Groups[1].Value -Field "recorded frame p95"
    $sortedRecordSamples = [double[]]@($recordSamples | Sort-Object)
    if ($recordMedian -ne $sortedRecordSamples[2]) {
        New-GateFailure "terminal-engine decision record median is not the third sorted sample"
    }
    if ($recordMedian -lt [double]$Fixture.minimum_mib_per_second -or
        $recordP95 -ge [double]$Fixture.maximum_frame_p95_ms) {
        New-GateFailure "terminal-engine decision record thresholds were not met"
    }
}

function New-RegressionFixture {
    return [pscustomobject]@{
        version = 1
        throughput_bytes = 104857600
        throughput_samples = 5
        minimum_mib_per_second = 40.0
        frame_cols = 120
        frame_rows = 40
        maximum_frame_p95_ms = 16.0
        scrollback_rows = 1000
        line_format = "scrollback-{index:04}"
        input_separator = "CRLF"
        input_trailing_crlf = $true
        canonicalization = "trim ASCII spaces from each rendered row and join rows with LF"
        sha256 = ("a" * 64)
    }
}

function New-RegressionOutput {
    $lines = @(
        $Header,
        "backend=$Backend",
        "throughput_bytes=104857600",
        "throughput_sample_1_mib_s=40.000000",
        "throughput_sample_2_mib_s=41.000000",
        "throughput_sample_3_mib_s=42.000000",
        "throughput_sample_4_mib_s=43.000000",
        "throughput_sample_5_mib_s=44.000000",
        "throughput_median_mib_s=42.000000",
        "frame_120x40_observations=1000",
        "frame_120x40_p95_ms=1.000000",
        "scrollback_rows=1000",
        "scrollback_sha256=$("a" * 64)",
        "decision=GO"
    )
    return $lines -join "`n"
}

function Invoke-RegressionProbe {
    param([Parameter(Mandatory)][string]$Probe)

    $fixture = New-RegressionFixture
    $valid = New-RegressionOutput
    try {
        $null = Assert-MeasurementOutput -Text $valid -Fixture $fixture
    }
    catch {
        [Console]::Error.WriteLine("terminal-engine regression baseline was rejected")
        exit 1
    }

    if ($Probe -ceq "measurement-failure") {
        $noGo = $valid.Replace("decision=GO", "decision=NO-GO")
        if (-not (Write-SanitizedMeasurementEvidence -Text $noGo)) {
            exit 1
        }
        if (Write-SanitizedMeasurementEvidence -Text "compiler output with a private path") {
            exit 1
        }
        exit 0
    }

    $invalid = $valid
    switch ($Probe) {
        "duplicate" { $invalid = "$valid`nthroughput_bytes=104857600" }
        "missing" { $invalid = $valid.Replace("throughput_sample_5_mib_s=44.000000`n", "") }
        "missing-equals" { $invalid = $valid.Replace("throughput_sample_1_mib_s=40.000000", "throughput_sample_1_mib_s 40.000000") }
        "malformed" { $invalid = $valid.Replace("throughput_sample_1_mib_s=40.000000", "throughput_sample_1_mib_s=malformed") }
        "nan" { $invalid = $valid.Replace("throughput_sample_1_mib_s=40.000000", "throughput_sample_1_mib_s=NaN") }
        "infinity" { $invalid = $valid.Replace("throughput_sample_1_mib_s=40.000000", "throughput_sample_1_mib_s=Infinity") }
        "negative" { $invalid = $valid.Replace("throughput_sample_1_mib_s=40.000000", "throughput_sample_1_mib_s=-1.000000") }
        "candidate" { $invalid = $valid.Replace("decision=GO", "decision=CANDIDATE") }
        "no-go" { $invalid = $valid.Replace("decision=GO", "decision=NO-GO") }
        "null" { $fixture.sha256 = $null }
    }
    $rejected = $false
    try {
        $null = Assert-MeasurementOutput -Text $invalid -Fixture $fixture
    }
    catch {
        $rejected = $true
    }
    if (-not $rejected) {
        [Console]::Error.WriteLine("terminal-engine regression probe accepted invalid output")
        exit 1
    }
    exit 0
}

if ($RegressionProbe.Length -gt 0) {
    Invoke-RegressionProbe -Probe $RegressionProbe
}

$RepoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\.."))
$FixturePath = Join-Path $RepoRoot "crates\rshell-session\tests\fixtures\vt\canary.json"
$RecordPath = Join-Path $RepoRoot "crates\rshell-session\TERMINAL_ENGINE.md"

try {
    $fixture = Read-CanaryFixture -Path $FixturePath
    $cargoArguments = @("bench", "-p", "rshell-session", "--bench", "terminal_engine", "--locked")
    Push-Location -LiteralPath $RepoRoot
    try {
        $captured = @(& cargo @cargoArguments 2>&1)
        $cargoExitCode = $LASTEXITCODE
    }
    finally {
        Pop-Location
    }
    $measurementText = @($captured | ForEach-Object { $_.ToString() }) -join "`n"
    if ($cargoExitCode -ne 0) {
        if ($null -eq $fixture.sha256) {
            New-GateFailure "terminal-engine fixture digest is unrecorded"
        }
        $null = Write-SanitizedMeasurementEvidence -Text $measurementText
        New-GateFailure "terminal-engine measurement command failed"
    }
    $measurement = Assert-MeasurementOutput -Text $measurementText -Fixture $fixture
    Assert-DecisionRecord -Path $RecordPath -Measurement $measurement -Fixture $fixture

    $Header
    foreach ($field in $FieldNames) {
        "$field=$($measurement.Values[$field])"
    }
}
catch [System.InvalidOperationException] {
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}
catch {
    [Console]::Error.WriteLine("terminal-engine gate failed")
    exit 1
}
