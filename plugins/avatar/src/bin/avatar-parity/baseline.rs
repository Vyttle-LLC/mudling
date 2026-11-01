//! The prototype's reading of the logs. The spike is frozen, so its sources
//! are copied and built elsewhere, with `parity/baseline.swift` beside them.
//! A run takes minutes, so its result is kept until an input changes.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

const EXPORTER: &str = include_str!("../../../parity/baseline.swift");
const PACKAGE: &str = r#"// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "ParityBaseline",
    platforms: [.macOS(.v15)],
    targets: [
        .target(name: "MUDKit"),
        .executableTarget(
            name: "parity-baseline",
            dependencies: ["MUDKit"],
            swiftSettings: [.swiftLanguageMode(.v5), .enableUpcomingFeature("BareSlashRegexLiterals")]
        ),
    ]
)
"#;

fn sorted_files(folder: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(folder)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_file())
        .collect();
    files.sort();
    Ok(files)
}

/// What the baseline depends on: its code and each log's name and size.
fn fingerprint(sources: &[PathBuf], logs: &[PathBuf]) -> std::io::Result<u64> {
    let mut hasher = DefaultHasher::new();
    EXPORTER.hash(&mut hasher);
    for source in sources {
        std::fs::read(source)?.hash(&mut hasher);
    }
    for log in logs {
        log.file_name().hash(&mut hasher);
        std::fs::metadata(log)?.len().hash(&mut hasher);
    }
    Ok(hasher.finish())
}

fn run(command: &mut Command) -> Result<String, String> {
    let output = command
        .output()
        .map_err(|error| format!("couldn't run {command:?}: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!("{command:?} failed:\n{stdout}\n{stderr}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Builds the copy and runs it over `logs`, unless that result is kept.
pub fn ensure(spike: &Path, logs: &[PathBuf], out: &Path) -> Result<PathBuf, String> {
    let sources_from = spike.join("Sources").join("MUDKit");
    let sources = sorted_files(&sources_from).map_err(|error| error.to_string())?;
    let key = fingerprint(&sources, logs).map_err(|error| error.to_string())?;
    let result = out.join(format!("baseline-{key:016x}.json"));
    if result.exists() {
        return Ok(result);
    }

    let copy = out.join("prototype-copy");
    let kit = copy.join("Sources").join("MUDKit");
    let exporter = copy.join("Sources").join("parity-baseline");
    let write_copy = || -> std::io::Result<()> {
        if kit.exists() {
            std::fs::remove_dir_all(&kit)?;
        }
        std::fs::create_dir_all(&kit)?;
        std::fs::create_dir_all(&exporter)?;
        for source in &sources {
            if let Some(name) = source.file_name() {
                std::fs::copy(source, kit.join(name))?;
            }
        }
        std::fs::write(exporter.join("main.swift"), EXPORTER)?;
        std::fs::write(copy.join("Package.swift"), PACKAGE)
    };
    write_copy().map_err(|error| error.to_string())?;

    eprintln!("building the prototype's reader from a copy…");
    let swift = |arguments: &[&str]| {
        let mut command = Command::new("swift");
        command.args(arguments).current_dir(&copy);
        run(&mut command)
    };
    swift(&["build", "-c", "release"])?;
    let bin = swift(&["build", "-c", "release", "--show-bin-path"])?;
    eprintln!("reading {} logs with it, which takes minutes…", logs.len());
    let mut command = Command::new(Path::new(&bin).join("parity-baseline"));
    run(command.arg(&result).args(logs))?;
    Ok(result)
}
