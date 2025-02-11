//! A library to inspect the CBOR output of `cargo criterion`
//!
//! The main entry point of this library is the [`read_trees()`] free function.
//! Point it to the root of a Cargo project or workspace where `cargo criterion`
//! has been run before in order to get started.

use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use criterion::Throughput;
#[cfg(doc)]
use criterion::{BenchmarkGroup, Criterion};
use serde::Deserialize;
use std::{
    ffi::OsStr,
    fs::DirEntry,
    io,
    path::{Path, PathBuf},
};

/// Enumerate the top-level benchmarks and benchmark groups from a certain Cargo
/// project or workspace
///
/// `cargo_root` should point to the location of Cargo's `target` directory,
/// i.e. to the workspace root in case of Cargo workspaces and the project
/// directory otherwise. Furthermore, `cargo criterion` should have recorded
/// at least one measurement before calling this function.
pub fn read_trees(
    cargo_root: impl AsRef<Path>,
) -> io::Result<impl Iterator<Item = io::Result<BenchmarkTree>>> {
    let mut criterion_data_root = cargo_root.as_ref().to_owned();
    criterion_data_root.push("target");
    criterion_data_root.push("criterion");
    criterion_data_root.push("data");
    // This is the "timeline" field of cargo-criterion's Model, which is
    // curently unused by cargo-criterion and always set to "main".
    criterion_data_root.push("main");
    let iter =
        std::fs::read_dir(criterion_data_root)?.map(|entry_res| entry_res.map(BenchmarkTree::new));
    Ok(iter)
}

/// Node of the `cargo criterion` data hierarchy
///
/// `cargo criterion` produces a filesystem tree of data where each node can
/// hold benchmark data and child nodes.
///
/// At least one of these must be present (a node must contain some benchmark
/// data or at least one child). But sadly they can both be present, because
/// `cargo criterion` doesn't guard against giving the same name to a benchmark
/// and a benchmark group.
#[derive(Debug)]
pub struct BenchmarkTree {
    root: DirEntry,
    measurements: Option<(Option<Box<Path>>, Vec<DirEntry>)>,
    children: Option<Vec<DirEntry>>,
}
//
/// Take one of the inner `Option`s if it's still available from a previous
/// `read_dir()` run, otherwise call `read_dir()` to fill it first then take it.
macro_rules! take_or_read {
    ($self_:ident.$vec:ident) => {
        if let Some($vec) = $self_.$vec.take() {
            Ok($vec)
        } else {
            $self_.read_dir().map(|()| {
                $self_
                    .$vec
                    .take()
                    .expect("Should have been filled by read_dir()")
            })
        }
    };
}
//
impl BenchmarkTree {
    /// Wrap a `DirEntry` after checking that it matches our expectations for
    /// `cargo-criterion`'s data directories.
    fn new(root: DirEntry) -> Self {
        debug_assert!(
            root.file_type().ok().is_none_or(|ty| ty.is_dir()),
            "Expected a benchmark data directory"
        );
        Self {
            root,
            measurements: None,
            children: None,
        }
    }

    /// Name of the directory holding this node's data
    ///
    /// This is a mangled form of the original Criterion group/function/input
    /// name, where filename-unsafe characters like `/` have been replaced with
    /// `_`.
    ///
    /// You can use this label to filter out data nodes at a minimal I/O cost.
    pub fn directory_name(&self) -> String {
        self.root
            .file_name()
            .into_string()
            .expect("Criterion data directories should have Unicode names")
    }

    /// Access the benchmark data from this node, if any
    pub fn read_benchmark(&mut self) -> io::Result<Option<Benchmark>> {
        let (metadata_path, measurements) = take_or_read!(self.measurements)?;
        let benchmark =
            metadata_path.map(move |metadata_path| Benchmark::new(metadata_path, measurements));
        Ok(benchmark)
    }

    /// Access the benchmark data from this node and all child nodes below it
    pub fn read_all_benchmarks(&mut self) -> io::Result<Vec<Benchmark>> {
        let mut benchmarks = self.read_benchmark()?.into_iter().collect::<Vec<_>>();
        let mut curr_children = self.read_children()?.collect::<Vec<_>>();
        let mut next_children = Vec::new();
        while !curr_children.is_empty() {
            for mut child in curr_children.drain(..) {
                benchmarks.extend(child.read_benchmark()?);
                next_children.extend(child.read_children()?);
            }
            std::mem::swap(&mut curr_children, &mut next_children);
        }
        Ok(benchmarks)
    }

    /// Enumerate the child nodes of this [`BenchmarkTree`]
    pub fn read_children(&mut self) -> io::Result<impl Iterator<Item = BenchmarkTree>> {
        Ok(take_or_read!(self.children)?
            .into_iter()
            .map(BenchmarkTree::new))
    }

    /// Read out the contents of this data directory, if not done already
    fn read_dir(&mut self) -> io::Result<()> {
        debug_assert!(self.measurements.is_none() || self.children.is_none());
        let root_path = self.root.path();
        let mut children = Vec::new();
        let mut measurements = Vec::new();
        let mut metadata_path = None;
        for entry in std::fs::read_dir(&root_path)? {
            let entry = entry?;
            match entry.file_type()? {
                ty if ty.is_dir() => {
                    children.push(entry);
                }
                ty if ty.is_file() => {
                    let file_name = entry.file_name();
                    if file_name == "benchmark.cbor" {
                        metadata_path = Some(entry.path().into_boxed_path());
                    } else {
                        let file_name = file_name
                            .to_str()
                            .expect("Criterion file names should be Unicode");
                        assert!(
                            file_name.starts_with("measurement_") && file_name.ends_with(".cbor"),
                            "Unexpected measurement file name"
                        );
                        measurements.push(entry);
                    }
                }
                other_ty => {
                    unreachable!(
                        "Encountered unexpected file type {other_ty:?} in Criterion data directory {}",
                        root_path.display()
                    );
                }
            }
        }
        assert_eq!(
            metadata_path.is_some(),
            !measurements.is_empty(),
            "Expecting benchmark.cbor if and only if measurements are present"
        );
        assert!(
            !(measurements.is_empty() && children.is_empty()),
            "Unexpected empty criterion data directory"
        );
        self.measurements = Some((metadata_path, measurements));
        self.children = Some(children);
        Ok(())
    }
}

/// Benchmark for which `cargo criterion` has recorded data
#[derive(Debug)]
pub struct Benchmark {
    metadata_path: Box<Path>,
    measurements: Vec<DirEntry>,
}
//
impl Benchmark {
    /// If a directory contains benchmark data, let the user access it
    fn new(metadata_path: Box<Path>, measurements: Vec<DirEntry>) -> Self {
        debug_assert!(
            metadata_path.exists() && !measurements.is_empty(),
            "Attempted to construct a Benchmark even though there's no measurements"
        );
        Self {
            metadata_path,
            measurements,
        }
    }

    /// Load this benchmark's metadata
    pub fn metadata(&self) -> io::Result<BenchmarkMetadata> {
        let data = std::fs::read(&self.metadata_path)?;
        Ok(serde_cbor::from_slice(&data[..]).expect("Failed to deserialize benchmark metadata"))
    }

    /// Enumerate this benchmark's measurements
    pub fn measurements(&self) -> impl Iterator<Item = io::Result<Measurement>> + '_ {
        self.measurements.iter().map(Measurement::new)
    }
}

/// Contents of a `benchmark.cbor` file from cargo-criterion
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct BenchmarkMetadata {
    /// Data which uniquely identifies a benchmark
    pub id: RawBenchmarkId,

    /// Path to the latest measurement. See also `latest_datetime`.
    pub latest_record: PathBuf,
}
//
impl BenchmarkMetadata {
    /// Low-resolution date and time of the latest measurement
    ///
    /// This is identical to [`Measurement::datetime()`] for the corresponding
    /// measurement, which can be used to locate it within the
    /// [`Benchmark::measurements`] iterator.
    ///
    /// A more precise timestamp (sub-second, UTC...) is available via
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
/// Metadata which uniquely identifies a benchmark
///
/// The interpretation of the fields of this struct heavily depends on how the
/// underlying Criterion benchmark was recorded.
///
/// In generic code that does not assume anything about the underlying
/// benchmarking procedure, it is recommended to use the
/// [`decode()`](Self::decode) method, which is the product of a careful
/// reverse-engineering of the Criterion benchmark identification rules.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct RawBenchmarkId {
    #[serde(rename = "group_id")]
    pub group_or_function_id: String,
    #[serde(rename = "function_id")]
    pub function_id_in_group: Option<String>,
    pub value_str: Option<String>,
    pub throughput: Option<Throughput>,
}
//
impl RawBenchmarkId {
    /// Decode the raw benchmark metadata into a higher-level view where field
    /// names are clearer and only valid combinations of fields are allowed.
    pub fn decode(&self) -> BenchmarkId<'_> {
        match (
            &self.function_id_in_group,
            &self.value_str,
            self.throughput.clone(),
        ) {
            // - Because both `function_id` and `value_str` are absent, we know
            //   that this benchmark is not part of a group. If the benchmark is
            //   part of a group, then `group_id` contains the group name, and
            //   at least one of these extra metadata must be specified so that
            //   group members can be differentiated from each other.
            // - Because `value_str` is absent, we know that
            //   `Criterion::bench_with_input()` has not been used, as it takes
            //   a `BenchmarkId` and Criterion users are only allowed to
            //   construct `BenchmarkId`s with a non-blank `parameter` value.
            // - Thus, by way of elimination, we know that
            //   `Criterion::bench_function()` was used.
            (None, None, None) => BenchmarkId::BenchFunction(&self.group_or_function_id),

            // As said above, if `function_id` and `value_str` are both absent,
            // this benchmark cannot be part of a group. Given that the
            // Criterion API does not let user specify throughput for
            // non-grouped benchmark, this metadata violates Criterion's
            // metadata schema and should be rejected.
            (None, None, Some(_)) => {
                unreachable!("Can't specify throughput in non-grouped Criterion benchmarks")
            }

            // - Because `throughput` is present, we know that a benchmark group
            //   was used (see above).
            // - Because `value_str` is present and `function_id` is absent, we
            //   know that the benchmark was identified within the group using a
            //   `BenchmarkId` that was constructed via
            //   `BenchmarkId::from_parameter()`.
            (None, Some(parameter), Some(throughput)) => BenchmarkId::InGroup {
                group_id: &self.group_or_function_id,
                member_id: MemberId::FromParameter(parameter),
                throughput: Some(throughput),
            },

            // - Because `throughput` and `function_id` are absent, we do not
            //   know whether a benchmark group was used or not.
            // - Because `group_id` is used for group names in benchmark groups
            //   but also for function names in `Criterion::bench_with_input()`,
            //   the metadata is ambiguous and we cannot tell whether this is...
            //   * ...a benchmark inside of a group, with a parameter value but
            //     no function name (benchmark ID constructed using
            //     `BenchmarkId::from_parameter`).
            //   * ...a benchmark outside of a group, with a function name and a
            //     value string (benchmark ID constructed using
            //     `BenchmarkId::new()`)
            (None, Some(parameter), None) => BenchmarkId::AmbiguousFromParameter {
                group_or_function_id: &self.group_or_function_id,
                parameter,
            },

            // - Because `function_id` is present, we know that this benchmark
            //   is part of a group
            // - Because `value_str` is absent, we know that the user did not
            //   use an explicit `BenchmarkId` constructor (all of which require
            //   specifying a parameter representation) and instead relied on
            //   implicit conversion of strings to benchmark identifiers.
            (Some(string), None, throughput) => BenchmarkId::InGroup {
                group_id: &self.group_or_function_id,
                member_id: MemberId::String(string),
                throughput,
            },

            // The only case both `function_id` and `value_str` are present is
            // if the benchmark is part of a group and the most general
            // `BenchmarkId::new()` ID construction method was used.
            (Some(function_name), Some(parameter), throughput) => BenchmarkId::InGroup {
                group_id: &self.group_or_function_id,
                member_id: MemberId::Full {
                    function_name,
                    parameter,
                },
                throughput,
            },
        }
    }
}
//
/// High-level interpretation of a [`RawBenchmarkId`]
pub enum BenchmarkId<'raw> {
    /// This benchmark was performed using [`Criterion::bench_function()`]
    BenchFunction(&'raw str),

    /// This benchmark was performed using one of the following procedures:
    ///
    /// - Directly called [`Criterion::bench_with_input()`] with `id` set to
    ///   [`BenchmarkId::new(group_or_function_id,
    ///   parameter)`](criterion::BenchmarkId::new).
    /// - [Created a benchmark group](Criterion::benchmark_group) with
    ///   `group_name` set to `group_or_function_id`, then called either
    ///   [`bench_function()`](BenchmarkGroup::bench_function) or
    ///   [`bench_with_input()`](BenchmarkGroup::bench_with_input) on the
    ///   resulting benchmark group with the `id` set to
    ///   [`BenchmarkId::from_parameter(parameter)`](criterion::BenchmarkId::from_parameter).
    ///
    /// Unfortunately, the criterion metadata schema is ambiguous and does not
    /// let us tell you which of these benchmarking procedures was used.
    AmbiguousFromParameter {
        /// If this benchmark is part of a group, then this is the group's name.
        /// Otherwise it is the function name of a `bench_with_input()`
        /// benchmark performed outside of a criterion group.
        group_or_function_id: &'raw str,

        /// String that identifies the benchmark input
        parameter: &'raw str,
    },

    /// This benchmark is part of a benchmark group
    InGroup {
        /// `group_name` that was passed to [`Criterion::benchmark_group()`]
        group_id: &'raw str,

        /// Textual identifier(s) of this benchmark inside of the group
        member_id: MemberId<'raw>,

        /// Throughput metadata for this benchmark, if any
        throughput: Option<Throughput>,
    },
}
//
/// Textual identifier(s) of this benchmark inside of the group
pub enum MemberId<'raw> {
    /// Textual identifier passed to [`BenchmarkGroup::bench_function()`] or
    /// [`BenchmarkGroup::bench_with_input()`]
    String(&'raw str),

    /// Parameter string that was generated using
    /// [`BenchmarkId::from_parameter()`].
    ///
    /// Please note that due to an ambiguity in Criterion's metadata schema, not
    /// all uses of benchmark groups with [`BenchmarkId::from_parameter()`] will
    /// be correctly classified into this enum variant. Instead, some of them
    /// will be classified as [`BenchmarkId::AmbiguousFromParameter`].
    ///
    /// [`BenchmarkId::from_parameter()`]: criterion::BenchmarkId::from_parameter
    FromParameter(&'raw str),

    /// Full benchmark identifier, featuring both a function name and parameter
    /// string, that was generated using
    /// [`BenchmarkId::new()`](criterion::BenchmarkId::new).
    Full {
        function_name: &'raw str,
        parameter: &'raw str,
    },
}

/// Criterion measurement from a specific benchmark
#[derive(Debug)]
pub struct Measurement<'parent> {
    entry: &'parent DirEntry,
    datetime: DateTime<Local>,
}
//
impl<'parent> Measurement<'parent> {
    /// Wrap a `DirEntry` after checking that it matches our expectations for
    /// `cargo-criterion`'s benchmark data directories.
    fn new(entry: &'parent DirEntry) -> io::Result<Self> {
        assert!(
            entry.file_type()?.is_file(),
            "Criterion's benchmark directories should only contain data files"
        );
        let datetime = parse_measurement_datetime(entry.file_name());
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
    /// The date and time of when these measurements were saved.
    pub datetime: DateTime<Utc>,
    /// The number of iterations in each sample
    pub iterations: Vec<f64>,
    /// The measured values from each sample
    pub values: Vec<f64>,
    /// The average values from each sample, ie. values / iterations
    pub avg_values: Vec<f64>,
    /// The statistical estimates from this run
    pub estimates: Estimates,
    /// The throughput of this run
    pub throughput: Option<Throughput>,
    /// The statistical differences compared to the last run. We save these so we
    /// don't have to recompute them later for the history report.
    pub changes: Option<ChangeEstimates>,
    /// Was the change (if any) significant?
    pub change_direction: Option<ChangeDirection>,

    /// An optional user-provided identifier string. This might be a version
    /// control commit ID or something custom
    pub history_id: Option<String>,
    /// An optional user-provided description. This might be a version control
    /// commit message or something custom.
    pub history_description: Option<String>,
}
//
/// Statistical estimates concerning a benchmark's iteration time
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct Estimates {
    pub mean: Estimate,
    pub median: Estimate,
    pub median_abs_dev: Estimate,
    pub slope: Option<Estimate>,
    pub std_dev: Estimate,
}
//
/// Statistical estimates concerning a change of benchmark iteration time
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct ChangeEstimates {
    pub mean: Estimate,
    pub median: Estimate,
}
//
/// Statistical estimate of some quantity
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct Estimate {
    /// The confidence interval for this estimate
    pub confidence_interval: ConfidenceInterval,
    /// The most likely value for this estimate
    pub point_estimate: f64,
    /// The standard error of this estimate
    pub standard_error: f64,
}
//
/// Confidence interval associated with a certain [`Estimate`]
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct ConfidenceInterval {
    pub confidence_level: f64,
    pub lower_bound: f64,
    pub upper_bound: f64,
}
//
/// Statistical change detected across benchmark runs
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
