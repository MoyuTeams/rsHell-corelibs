#[cfg(windows)]
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("rshell-session must be under the workspace crates directory")
        .to_path_buf()
}

#[cfg(windows)]
#[test]
fn windows_pty_resolves_the_vendored_source() {
    let root = workspace_root();
    let metadata = Command::new("cargo")
        .args(["metadata", "--locked", "--format-version", "1"])
        .current_dir(&root)
        .output()
        .expect("run cargo metadata for selected PTY package");
    assert!(
        metadata.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&metadata.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&metadata.stdout).expect("parse cargo metadata");
    let packages = metadata["packages"].as_array().expect("metadata packages");
    let selected = packages
        .iter()
        .filter(|package| package["name"] == "portable-pty-psmux")
        .collect::<Vec<_>>();
    assert_eq!(selected.len(), 1, "exactly one PTY package must resolve");
    assert_eq!(selected[0]["version"], "0.9.6");
    let manifest = Path::new(
        selected[0]["manifest_path"]
            .as_str()
            .expect("selected PTY manifest path"),
    );
    assert_eq!(
        manifest,
        root.join("third_party/portable-pty-psmux/Cargo.toml"),
        "the selected package must be the narrow vendored 0.9.6 source"
    );

    let vendor = root.join("third_party/portable-pty-psmux");
    let mut vendored_files = Vec::new();
    collect_relative_files(&vendor, &vendor, &mut vendored_files);
    assert_eq!(
        vendored_files.into_iter().collect::<BTreeSet<_>>(),
        [
            "Cargo.toml",
            "LICENSE.md",
            "README.md",
            "README.rshell-patch.md",
            "examples/bash.rs",
            "examples/narrow.rs",
            "examples/whoami.rs",
            "examples/whoami_async.rs",
            "src/cmdbuilder.rs",
            "src/lib.rs",
            "src/serial.rs",
            "src/unix.rs",
            "src/win/conpty.rs",
            "src/win/mod.rs",
            "src/win/procthreadattr.rs",
            "src/win/psuedocon.rs",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        "only the selected package files and patch provenance may be vendored"
    );
    let provenance = std::fs::read_to_string(vendor.join("README.rshell-patch.md")).unwrap();
    assert!(
        provenance.contains("793e46fb3212b514f6eb694e26a64aeaca64b47a2d66b810351b44628e307a0e")
    );
    for path in [
        "src/lib.rs",
        "src/win/procthreadattr.rs",
        "src/win/psuedocon.rs",
        "src/win/conpty.rs",
    ] {
        assert!(
            provenance.contains(path),
            "missing patch provenance for {path}"
        );
    }
}

#[cfg(windows)]
fn collect_relative_files(root: &Path, directory: &Path, files: &mut Vec<String>) {
    let mut entries = std::fs::read_dir(directory)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_relative_files(root, &path, files);
        } else {
            files.push(
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}

#[test]
fn terminal_engine_gate_contract_is_exact() {
    let root = workspace_root();
    let session = root.join("crates/rshell-session");
    let manifest = std::fs::read_to_string(session.join("Cargo.toml")).unwrap();
    assert!(manifest.contains("name = \"terminal_engine\""));
    assert!(manifest.contains("sha2 = \"0.10.9\""));
    assert!(!manifest.contains("name = \"throughput\""));
    assert!(!session.join("benches/throughput.rs").exists());

    let bench = std::fs::read_to_string(session.join("benches/terminal_engine.rs"))
        .expect("the exact terminal-engine measurement executable must exist");
    let script = std::fs::read_to_string(root.join("scripts/qa/terminal-engine-gate.ps1"))
        .expect("the fail-closed terminal-engine gate must exist");
    let fixture = std::fs::read_to_string(session.join("tests/fixtures/vt/canary.json"))
        .expect("the terminal-engine canary fixture must exist");
    let record = std::fs::read_to_string(session.join("TERMINAL_ENGINE.md"))
        .expect("the terminal-engine decision record must exist");

    let mut fixture: serde_json::Value = serde_json::from_str(&fixture).unwrap();
    let fixture_digest = fixture["sha256"].clone();
    match &fixture_digest {
        serde_json::Value::Null => {}
        serde_json::Value::String(digest) => assert!(
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "a recorded fixture digest must be 64 lowercase hex characters"
        ),
        value => panic!("fixture digest must be null or a string, got {value:?}"),
    }
    fixture["sha256"] = serde_json::Value::Null;
    assert_eq!(
        fixture,
        serde_json::json!({
            "version": 1,
            "throughput_bytes": 104857600,
            "throughput_samples": 5,
            "minimum_mib_per_second": 40.0,
            "frame_cols": 120,
            "frame_rows": 40,
            "maximum_frame_p95_ms": 16.0,
            "scrollback_rows": 1000,
            "line_format": "scrollback-{index:04}",
            "input_separator": "CRLF",
            "input_trailing_crlf": true,
            "canonicalization": "trim ASCII spaces from each rendered row and join rows with LF",
            "sha256": null
        })
    );

    for exact in [
        "104857600",
        "5",
        "40.0",
        "120",
        "40",
        "1000",
        "16.0",
        "--record-candidate",
        "decision=CANDIDATE",
        "decision=NO-GO",
        "decision=GO",
        "print_measurements",
        "[argument] if argument == \"--bench\"",
        "candidate == \"--record-candidate\" && bench == \"--bench\"",
    ] {
        assert!(bench.contains(exact), "bench is missing `{exact}`");
    }
    for trace_fragment in [
        "rsHell throughput",
        "α界e\\u{301}",
        "\\x1b[31mRED\\x1b[0m",
        "\\x1b[1;34mBOLD-BLUE\\x1b[0m",
        "\\r\\n",
    ] {
        assert!(
            bench.contains(trace_fragment),
            "throughput trace is missing representative fragment `{trace_fragment}`"
        );
    }
    let throughput_measurement = bench.find("let samples = measure_throughput").unwrap();
    let frame_measurement = bench.find("let frame_p95_ms = measure_frame_p95").unwrap();
    let correctness_oracle = bench
        .find("let digest = verify_scrollback_and_hash()?")
        .unwrap();
    let threshold_decision = bench
        .find("if !throughput_passed || !frame_passed")
        .unwrap();
    let passing_decision = bench
        .find("let decision = if mode == Mode::RecordCandidate")
        .unwrap();
    assert!(throughput_measurement < frame_measurement);
    assert!(frame_measurement < correctness_oracle);
    assert!(correctness_oracle < threshold_decision);
    assert!(threshold_decision < passing_decision);
    for field in [
        "RSHELL_TERMINAL_ENGINE_GATE version=1",
        "backend=alacritty-terminal@0.26.0",
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
        "decision",
    ] {
        assert!(bench.contains(field), "bench is missing `{field}`");
        let script_field = field.strip_prefix("backend=").unwrap_or(field);
        assert!(
            script.contains(script_field),
            "gate script is missing `{script_field}`"
        );
    }
    assert!(script.contains("cargo bench -p rshell-session --bench terminal_engine --locked"));
    assert!(script.contains("$recordSamples = @()"));
    assert!(script.contains("$recordMedian -ne $sortedRecordSamples[2]"));
    assert!(script.contains("terminal-engine decision record thresholds were not met"));
    assert!(!script.contains("$Measurement.Values[$SampleFields[$index]]"));
    let sanitized_failure = script
        .find("Write-SanitizedMeasurementEvidence -Text $measurementText")
        .expect("a failed measurement must emit only closed terminal-engine fields");
    let generic_failure = script
        .find("New-GateFailure \"terminal-engine measurement command failed\"")
        .unwrap();
    assert!(sanitized_failure < generic_failure);
    assert!(record.contains("`decision=NO-GO`"));
    match fixture_digest {
        serde_json::Value::Null => {
            assert!(record.contains("Decision: **NO-GO (unrecorded)**"));
            assert!(record.contains("sha256: null"));
            assert!(!record.contains("Decision: **GO**"));
        }
        serde_json::Value::String(digest) => {
            assert!(record.contains("Decision: **GO**"));
            assert!(record.contains(&digest));
            assert!(!record.contains("Decision: **NO-GO (unrecorded)**"));
        }
        _ => unreachable!(),
    }
    assert!(!record.contains("TODO"));
    assert!(!record.contains("TBD"));

    for probe in [
        "duplicate",
        "missing",
        "missing-equals",
        "malformed",
        "nan",
        "infinity",
        "negative",
        "candidate",
        "no-go",
        "null",
        "measurement-failure",
    ] {
        let output = Command::new("pwsh")
            .args([
                "-NoProfile",
                "-File",
                "scripts/qa/terminal-engine-gate.ps1",
                "-RegressionProbe",
                probe,
            ])
            .current_dir(&root)
            .output()
            .expect("PowerShell must launch the terminal-engine parser probe");
        assert!(
            output.status.success(),
            "terminal-engine parser probe {probe} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
