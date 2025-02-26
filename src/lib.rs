//! A library to access the CBOR output of `cargo criterion`
//!
//! The main entry point of this library is [`Search::in_cargo_root()`]. Point
//! it to the root of a Cargo project or workspace, then call the
//! [`find_all()`](Search::find_all) or
//! [`find_in_paths()`](Search::find_in_paths) method of the resulting object to
//! start enumerating data.

use chrono::{DateTime, Local, MappedLocalTime, NaiveDateTime, TimeZone, Utc};
use criterion::Throughput;
#[cfg(doc)]
use criterion::{BenchmarkGroup, Criterion};
use serde::Deserialize;
use std::{
    cmp::Ordering,
    ffi::OsStr,
    io,
    iter::Peekable,
    path::{Path, PathBuf},
};
use walkdir::{DirEntry, WalkDir};

/// Criterion benchmark data search
///
/// You start a search with [`Search::in_cargo_root()`], which allows you to
/// specify where the `target` directory of the project is located.
#[derive(Debug)]
pub struct Search {
    data_root: Box<Path>,
    walker: walkdir::IntoIter,
}
//
impl Search {
    /// Start by specifying the Cargo hierarchy root
    ///
    /// For Cargo workspaces, this will be the root of the workspace. For
    /// non-workspace projects, this will be the root of the Cargo project,
    /// where the `Cargo.toml` file is located.
    ///
    /// # Panics
    ///
    /// If the specified directory does not exist.
    pub fn in_cargo_root(cargo_root: impl AsRef<Path>) -> Self {
        // Find the Criterion data root
        let cargo_root = cargo_root.as_ref();
        assert!(cargo_root.exists(), "Specified Cargo root does not exist");
        Self::in_target_dir(cargo_root.join("target"))
    }

    /// Start by specifying the target directory location
    ///
    /// Like [`in_cargo_root()`](Self::in_cargo_root()), but you directly
    /// specify the path to the `target` directory, which must already exist.
    ///
    /// # Panics
    ///
    /// If the specified directory does not exist.
    pub fn in_target_dir(target_path: impl AsRef<Path>) -> Self {
        // Find the Criterion data root
        let target_path = target_path.as_ref();
        assert!(
            target_path.exists(),
            "Specified target directory does not exist"
        );
        let mut data_root = target_path.to_owned();
        data_root.push("criterion");
        data_root.push("data");
        // This is the "timeline" field of cargo-criterion's Model, which is
        // curently unused by cargo-criterion and always set to "main".
        data_root.push("main");
        let data_root = data_root.into_boxed_path();

        // Set up the common directory-walking configuration
        let walker = WalkDir::new(&data_root)
            .min_depth(1)
            .follow_root_links(false)
            .sort_by(|entry1, entry2| {
                // - Emit all files before emitting directories
                // - Emit files in descending name order (this will yield all
                //   measurement_xxx.cbor files first, sorted by decreasing
                //   measurement date/time to put latest measurement first, then
                //   the benchmark.cbor metadata file at the end)
                // - Emit directories in ascending name order
                let is_file_not_dir = |entry: &DirEntry| -> bool {
                    let ty = entry.file_type();
                    assert!(
                        ty.is_dir() || ty.is_file(),
                        "Criterion's data directory should only contain files and directories"
                    );
                    ty.is_file()
                };
                match (is_file_not_dir(entry1), is_file_not_dir(entry2)) {
                    // Files before directories
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    // Files in descending name order
                    (true, true) => entry2.file_name().cmp(entry1.file_name()),
                    // Directories in ascending name order
                    (false, false) => entry1.file_name().cmp(entry2.file_name()),
                }
            })
            .into_iter();
        Self { data_root, walker }
    }

    /// Find all benchmark data in the specified Cargo project/workspace
    pub fn find_all(self) -> impl Iterator<Item = walkdir::Result<Benchmark>> {
        BenchmarkIter::new(self.data_root, self.walker)
    }

    /// Find benchmark data whose filesystem path matches a certain predicate
    ///
    /// Criterion organizes benchmark data into a filesystem hierarchy that
    /// roughly matches the benchmark groups, function names and value strings
    /// that were specified in the benchmark (with various subtleties). If you
    /// know where the data that you are looking for is located, you can use
    /// this variant of `find_all()` to restrict the filesystem walk to these
    /// directories only.
    ///
    /// To cut off useless filesystem walk branches as early as possible, the
    /// predicate will be successively called for each component of the
    /// filesystem path. So if you want to select benchmark data from location
    /// `a/b/c` only, your filter should successively match for three
    /// directories: first `a` at depth 1, then `b` at depth 2, and finally `c`
    /// at depth 3.
    pub fn find_in_paths<'path_filter>(
        self,
        mut path_filter: impl FnMut(DataDirectory) -> bool + 'path_filter,
    ) -> impl Iterator<Item = walkdir::Result<Benchmark>> + 'path_filter {
        let data_root = self.data_root.clone();
        let walker = self.walker.filter_entry(move |entry| {
            if entry.file_type().is_dir() {
                path_filter(DataDirectory::new(&data_root, entry))
            } else {
                true
            }
        });
        BenchmarkIter::new(self.data_root, walker)
    }
}

/// Criterion benchmark data directory
#[derive(Debug)]
pub struct DataDirectory<'dirwalk> {
    data_root: &'dirwalk Path,
    entry: &'dirwalk DirEntry,
}
//
impl<'dirwalk> DataDirectory<'dirwalk> {
    /// Wrap a directory entry from [`WalkDir`] in a nice façade that hides
    /// user-irrelevant details
    fn new(data_root: &'dirwalk Path, entry: &'dirwalk DirEntry) -> Self {
        debug_assert!(!entry.path_is_symlink() && entry.file_type().is_dir() && entry.depth() > 0);
        Self { data_root, entry }
    }

    /// Name of this data directory (without the path leading to it)
    pub fn dir_name(&self) -> &str {
        self.entry
            .file_name()
            .to_str()
            .expect("Criterion should not generate non-Unicode names")
    }

    /// Depth at which this data directory appears
    ///
    /// Top-level data directories have depth 1, their children have depth 2,
    /// their grandchildren have depth 3, and so on.
    pub fn depth(&self) -> usize {
        self.entry.depth()
    }

    /// Relative path to this data directory from the Criterion data root
    pub fn path_from_data_root(&self) -> &Path {
        self.entry
            .path()
            .strip_prefix(self.data_root)
            .expect("Walkdir should prefix entry paths with the search root path")
    }
}

/// Benchmark iterator
///
/// Wraps a walkdir iterator by adding a layer that collects all the files from
/// the current directory before yielding them as a single [`Benchmark`].
struct BenchmarkIter<Walker: Iterator> {
    /// Root of the directory walk
    data_root: Box<Path>,

    /// Underlying directory walker
    walker: Peekable<Walker>,

    /// Files seen so far in the current directory
    files_in_current_dir: Vec<DirEntry>,

    /// There is no benchmark data and this iterator should always yield None
    ///
    /// This is used to work around the fact that `walkdir` returns errors when
    /// the data directory to be walked does not exists, whereas we want to
    /// treat this as a normal situation where there is no benchmark data.
    no_data: bool,
}
//
impl<Walker: Iterator> BenchmarkIter<Walker> {
    /// Set up a benchmark iterator
    ///
    /// This is an implementation detail of [`Search`], and it is assumed that
    /// all preparations from [`Search::in_cargo_root()`] have been done.
    fn new(data_root: Box<Path>, walker: Walker) -> Self {
        let no_data = !data_root.exists();
        BenchmarkIter {
            data_root,
            walker: walker.peekable(),
            files_in_current_dir: Vec::new(),
            no_data,
        }
    }

    /// Reached end of file list for current depth, produce a Benchmark from it
    fn emit_benchmark(&mut self) -> Option<walkdir::Result<Benchmark>> {
        // Last file will be benchmark.cbor due to the sorting we applied
        let metadata = self.files_in_current_dir.pop()?;
        let measurements = std::mem::take(&mut self.files_in_current_dir).into_boxed_slice();
        Some(Ok(Benchmark::new(&self.data_root, metadata, measurements)))
    }
}
//
impl<Walker> Iterator for BenchmarkIter<Walker>
where
    Walker: Iterator<Item = walkdir::Result<DirEntry>>,
{
    type Item = walkdir::Result<Benchmark>;
    fn next(&mut self) -> Option<Self::Item> {
        // Yield None if there is no benchmark data
        if self.no_data {
            return None;
        }

        // Otherwise, collect files from the next `Benchmark`, if any
        'files: loop {
            // Fetch next entry from dir walker, handling errors and end-of-walk
            let entry = match self.walker.peek() {
                Some(Ok(entry)) => entry,
                Some(Err(_)) => {
                    return self
                        .walker
                        .next()
                        .map(|err| err.map(|_| unreachable!("Peeked Err() above")))
                }
                None => return self.emit_benchmark(),
            };

            // Makes sure entries meet expectations
            let ty = entry.file_type();
            assert!(
                !entry.path_is_symlink(),
                "No symlink expected in Criterion data directory"
            );
            assert!(
                ty.is_file() || ty.is_dir(),
                "Only files & subdirectories expected inside of Criterion data directory"
            );
            debug_assert!(
                entry.depth() >= 1,
                "Root directory should filtered out by min_depth"
            );

            // Are we currently collecting files from a benchmark?
            if let Some(last_file) = self.files_in_current_dir.last() {
                if entry.depth() == last_file.depth()
                    && entry.file_type().is_file()
                    && entry.path().parent() == last_file.path().parent()
                {
                    // This is a file from the same benchmark directory, add it
                    // to the list and keep checking next files.
                    self.files_in_current_dir.push(
                        self.walker
                            .next()
                            .expect("Peeked Some() above")
                            .expect("Peeked Ok() above"),
                    );
                    continue 'files;
                } else {
                    // This not a file from the same benchmark directory. Flush
                    // all files seen so far into a new benchmark, and yield
                    // that benchmark. We'll get back to the current entry next
                    // time Iterator::next() is called.
                    return self.emit_benchmark();
                }
            }

            // If control reached this point, then the files_in_current_dir list
            // is empty and we are not going to emit a benchmark. So we can
            // commit to popping the entry from the iterator.
            assert!(self.files_in_current_dir.is_empty());
            let entry = self
                .walker
                .next()
                .expect("Peeked Some() above")
                .expect("Peeked Ok() above");

            // If this is a file, start a new benchmark. Ignore directories.
            if ty.is_file() {
                self.files_in_current_dir.push(entry);
            }
        }
    }
}

/// Benchmark for which `cargo criterion` has recorded data
#[derive(Debug)]
pub struct Benchmark {
    path_from_data_root: Box<Path>,
    metadata: DirEntry,
    measurements: Box<[DirEntry]>,
}
//
impl Benchmark {
    /// If a directory contains benchmark data, let the user access it
    fn new(data_root: &Path, metadata: DirEntry, measurements: Box<[DirEntry]>) -> Self {
        assert!(
            metadata.file_type().is_file() && metadata.file_name() == "benchmark.cbor",
            "Encountered unexpected file {metadata:?} in Criterion data directory"
        );
        assert!(
            !measurements.is_empty(),
            "Attempted to construct a Benchmark even though there's no measurements"
        );
        let parent_dir = metadata
            .path()
            .parent()
            .expect("Detected benchmark.cbor file should lie inside a parent directory");
        let path_from_data_root = parent_dir.strip_prefix(data_root).expect(
            "Detected benchmark.cbor file should be inside of the Criterion data directory root",
        );
        Self {
            path_from_data_root: path_from_data_root.into(),
            metadata,
            measurements,
        }
    }

    /// Relative path to this benchmark's data directory from the Criterion data root
    pub fn path_from_data_root(&self) -> &Path {
        &self.path_from_data_root
    }

    /// Read this benchmark's metadata
    pub fn metadata(&self) -> io::Result<BenchmarkMetadata> {
        let data = std::fs::read(self.metadata.path())?;
        Ok(serde_cbor::from_slice(&data[..]).expect("Failed to deserialize benchmark metadata"))
    }

    /// Enumerate this benchmark's measurements
    pub fn measurements(&self) -> impl Iterator<Item = Measurement> + '_ {
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
    /// Local date and time of the latest measurement
    ///
    /// This is identical to [`Measurement::local_datetime()`] for the
    /// corresponding measurement, which can be used to locate said measurement
    /// within the [`Benchmark::measurements`] iterator.
    ///
    /// A more precise timestamp (sub-second, UTC...) can be found inside of
    /// individual measurement files via [`MeasurementData::datetime`].
    pub fn latest_local_datetime(&self) -> MappedLocalTime<DateTime<Local>> {
        parse_measurement_datetime(
            self.latest_record
                .file_name()
                .expect("Latest record field should point to a measurement file"),
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
#[derive(Clone, Debug, Eq, PartialEq)]
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
    /// let us tell you which of these benchmarking procedures was used without
    /// supplementary information.
    AmbiguousFromParameter {
        /// If this benchmark is part of a group, then this is the group's name.
        /// Otherwise it is the `function_name` that was passed to
        /// [`BenchmarkId::new()`](criterion::BenchmarkId::new) at the time
        /// where [`Criterion::bench_with_input()`] was called.
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemberId<'raw> {
    /// Textual identifier that was passed to
    /// [`BenchmarkGroup::bench_function()`] or
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

    /// Full benchmark identifier, featuring both a function name and a
    /// parameter identification string, that was generated using
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
}
//
impl<'parent> Measurement<'parent> {
    /// Wrap a `DirEntry` after checking that it matches our expectations for
    /// `cargo-criterion`'s benchmark data directories.
    fn new(entry: &'parent DirEntry) -> Self {
        assert!(
            entry.file_type().is_file(),
            "Criterion's benchmark directories should only contain data files"
        );
        Self { entry }
    }

    /// Local date and time at which this measurement was taken
    pub fn local_datetime(&self) -> MappedLocalTime<DateTime<Local>> {
        parse_measurement_datetime(self.entry.file_name())
    }

    /// Read this measurement's data
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
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub enum ChangeDirection {
    NoChange,
    NotSignificant,
    Improved,
    Regressed,
}

/// Parse a measurement file name to find the measurement date & time
fn parse_measurement_datetime(file_name: impl AsRef<OsStr>) -> MappedLocalTime<DateTime<Local>> {
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
    Local.from_local_datetime(&datetime)
}
