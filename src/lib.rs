//! A library to inspect the CBOR output of `cargo criterion`
//!
//! The main entry point of this library is the [`read_groups()`] free function.
//! Point it to the root of a Cargo project or workspace where `cargo criterion`
//! has been run before to get started.

use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use criterion::Throughput;
use serde::Deserialize;
use std::{
    ffi::{OsStr, OsString},
    fs::DirEntry,
    io,
    path::{Path, PathBuf},
};

/// Enumerate benchmark groups for which Criterion has recorded data
///
/// `cargo_root` is the root of a Cargo project or workspace, within which the
/// project's `target` folder is located.
pub fn read_groups(
    cargo_root: impl AsRef<Path>,
) -> io::Result<impl Iterator<Item = io::Result<BenchmarkGroup>>> {
    let mut criterion_data_root = cargo_root.as_ref().to_owned();
    criterion_data_root.push("target");
    criterion_data_root.push("criterion");
    criterion_data_root.push("data");
    criterion_data_root.push("main"); // Unused "timeline" tunable
    Ok(std::fs::read_dir(criterion_data_root)?
        .map(|entry_res| entry_res.and_then(BenchmarkGroup::new)))
}

/// Benchmark group for which Criterion has recorded data
#[derive(Debug)]
pub struct BenchmarkGroup(DirEntry);
//
impl BenchmarkGroup {
    /// Wrap a `DirEntry` after checking that it matches our expectations for
    /// `cargo-criterion`'s benchmark group data directories.
    fn new(entry: DirEntry) -> io::Result<Self> {
        assert!(
            entry.file_type()?.is_dir(),
            "Criterion's timeline directory should only contain group subdirectories"
        );
        Ok(Self(entry))
    }

    /// Name of the directory holding this benchmark group's data
    ///
    /// This is a mangled form of the original Criterion benchmark group name,
    /// where filename-unsafe characters like '/' have been replaced with '_'.
    ///
    /// You can use this to filter out benchmark groups before enumerating
    /// benchmarks within them with [`benchmarks()`](Self::benchmarks).
    pub fn directory_name(&self) -> String {
        self.0
            .file_name()
            .into_string()
            .expect("Criterion benchmark group directories should have Unicode names")
    }

    /// Enumerate benchmarks within the group
    pub fn benchmarks(&self) -> io::Result<impl Iterator<Item = io::Result<Benchmark>>> {
        Ok(std::fs::read_dir(self.0.path())?.map(|entry_res| entry_res.and_then(Benchmark::new)))
    }
}

/// Benchmark for which Criterion has recorded data
#[derive(Debug)]
pub struct Benchmark(DirEntry);
//
impl Benchmark {
    /// Wrap a `DirEntry` after checking that it matches our expectations for
    /// `cargo-criterion`'s benchmark group data directories.
    fn new(entry: DirEntry) -> io::Result<Self> {
        assert!(
            entry.file_type()?.is_dir(),
            "Criterion's benchmark group directories should only contain benchmark subdirectories"
        );
        Ok(Self(entry))
    }

    /// Name of the directory holding this benchmark's data
    ///
    /// This is a mangled form of the original Criterion benchmark name,
    /// where filename-unsafe characters like '/' have been replaced with '_'.
    ///
    /// You can use this to filter out benchmarks before reading their metadata
    /// with [`metatada()`](Self::metadata) or enumerating measurements with
    pub fn directory_name(&self) -> String {
        self.0
            .file_name()
            .into_string()
            .expect("Criterion benchmark directories should have Unicode names")
    }

    /// Load this benchmark's metadata
    pub fn metadata(&self) -> io::Result<BenchmarkMetadata> {
        let mut path = self.0.path();
        path.push("benchmark.cbor");
        let data = std::fs::read(path)?;
        Ok(serde_cbor::from_slice(&data[..]).expect("Failed to deserialize benchmark metadata"))
    }

    /// Enumerate this benchmark's measurements
    pub fn measurements(&self) -> io::Result<impl Iterator<Item = io::Result<Measurement>>> {
        let bench_dir = std::fs::read_dir(self.0.path())?;
        let iter = bench_dir.filter_map(|entry_res| match entry_res {
            Ok(entry) => {
                let name = entry.file_name();
                (name != "benchmark.cbor").then(move || Measurement::new(entry, name))
            }
            Err(e) => Some(Err(e)),
        });
        Ok(iter)
    }
}

/// Contents of a `benchmark.cbor` file from cargo-criterion
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct BenchmarkMetadata {
    pub id: BenchmarkId,
    pub latest_record: PathBuf,
}
//
impl BenchmarkMetadata {
    /// Low-resolution date and time of the latest measurement, as reported by
    /// [`Measurement::datetime()`] and more precisely reported by
    /// [`MeasurementData::datetime`].
    pub fn latest_datetime(&self) -> DateTime<Local> {
        parse_measurement_datetime(
            self.latest_record
                .file_name()
                .expect("Latest record should be a file"),
        )
    }
}
//
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct BenchmarkId {
    pub group_id: String,
    pub function_id: Option<String>,
    pub value_str: Option<String>,
    pub throughput: Option<Throughput>,
}

/// Criterion measurement from a specific benchmark
#[derive(Debug)]
pub struct Measurement {
    entry: DirEntry,
    datetime: DateTime<Local>,
}
//
impl Measurement {
    /// Wrap a `DirEntry` after checking that it matches our expectations for
    /// `cargo-criterion`'s benchmark data directories.
    fn new(entry: DirEntry, file_name: OsString) -> io::Result<Self> {
        assert!(
            entry.file_type()?.is_file(),
            "Criterion's benchmark directories should only contain data files"
        );
        let datetime = parse_measurement_datetime(file_name);
        Ok(Self { entry, datetime })
    }

    /// Date and time at which this measurement was taken
    pub fn datetime(&self) -> DateTime<Local> {
        self.datetime
    }

    /// Data from this measurement
    pub fn data(&self) -> io::Result<MeasurementData> {
        let data = std::fs::read(self.entry.path())?;
        Ok(serde_cbor::from_slice(&data[..]).expect("Failed to deserialize benchmark metadata"))
    }
}

/// Contents of a `measurement_<datetime>.cbor` file from cargo-criterion
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct MeasurementData {
    // The date and time of when these measurements were saved.
    pub datetime: DateTime<Utc>,
    // The number of iterations in each sample
    pub iterations: Vec<f64>,
    // The measured values from each sample
    pub values: Vec<f64>,
    // The average values from each sample, ie. values / iterations
    pub avg_values: Vec<f64>,
    // The statistical estimates from this run
    pub estimates: Estimates,
    // The throughput of this run
    pub throughput: Option<Throughput>,
    // The statistical differences compared to the last run. We save these so we
    // don't have to recompute them later for the history report.
    pub changes: Option<ChangeEstimates>,
    // Was the change (if any) significant?
    pub change_direction: Option<ChangeDirection>,

    // An optional user-provided identifier string. This might be a version
    // control commit ID or something custom
    pub history_id: Option<String>,
    // An optional user-provided description. This might be a version control
    // commit message or something custom.
    pub history_description: Option<String>,
}
//
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct Estimates {
    pub mean: Estimate,
    pub median: Estimate,
    pub median_abs_dev: Estimate,
    pub slope: Option<Estimate>,
    pub std_dev: Estimate,
}
//
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct ChangeEstimates {
    pub mean: Estimate,
    pub median: Estimate,
}
//
//
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct Estimate {
    /// The confidence interval for this estimate
    pub confidence_interval: ConfidenceInterval,
    //
    pub point_estimate: f64,
    /// The standard error of this estimate
    pub standard_error: f64,
}
//
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct ConfidenceInterval {
    pub confidence_level: f64,
    pub lower_bound: f64,
    pub upper_bound: f64,
}
//
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub enum ChangeDirection {
    NoChange,
    NotSignificant,
    Improved,
    Regressed,
}

/// Parse a measurement file name to find the measurement date & time
fn parse_measurement_datetime(file_name: impl AsRef<OsStr>) -> DateTime<Local> {
    let datetime = file_name
        .as_ref()
        .to_str()
        .expect("Measurement file name should be Unicode")
        .strip_prefix("measurement_")
        .expect("Measurement file name should start with measurement_")
        .strip_suffix(".cbor")
        .expect("Measurement file name should end with .cbor extension");
    let datetime = NaiveDateTime::parse_from_str(datetime, "%y%m%d%H%M%S")
        .expect("Unexpected criterion measurement date/time format");
    Local.from_local_datetime(&datetime).unwrap()
}
