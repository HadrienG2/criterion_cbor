//! This utility dumps the data recorded by criterion inside of a certain
//! project, while also checking the invariants that criterion-cbor makes about
//! the behavior of cargo-criterion.

use chrono::TimeDelta;
use std::path::PathBuf;

fn main() {
    assert!(
        (1..=2).contains(&std::env::args_os().count()),
        "Only expected 1-2 OS arguments (program exe + optional target path)"
    );
    let cargo_root = std::env::args_os().nth(1).map_or_else(
        || std::env::current_dir().expect("Failed to access current dir"),
        PathBuf::from,
    );
    for group in
        criterion_cbor::read_groups(cargo_root).expect("Failed to start reading benchmark groups")
    {
        let group = group.expect("Failed to open a benchmark group's directory");
        let group_directory = group.directory_name();
        println!("\n### Loading benchmark group from directory {group_directory} ###");

        let mut last_group_id = None;
        for bench in group
            .benchmarks()
            .expect("Failed to start reading benchmarks")
        {
            let bench = bench.expect("Failed to open a benchmark's directory");
            let bench_directory = bench.directory_name();
            println!("\n=== Loading benchmark from subdirectory {bench_directory} ===\n");

            let metadata = bench.metadata().expect("Failed to read benchmark metadata");
            println!("{metadata:#?}\n");

            let make_filename_safe = |s: &str| {
                s.replace(
                    &['?', '"', '/', '\\', '*', '<', '>', ':', '|', '^'][..],
                    "_",
                )
            };
            if let Some(last_group_id) = last_group_id.as_ref() {
                assert_eq!(
                    &metadata.id.group_id, last_group_id,
                    "Group ID doesn't match with other benchmarks in group"
                );
            } else {
                let expected_group_directory = make_filename_safe(&metadata.id.group_id);
                assert_eq!(group_directory, expected_group_directory);
                last_group_id = Some(metadata.id.group_id.to_owned())
            }
            let expected_bench_directory = make_filename_safe(
                metadata
                    .id
                    .function_id
                    .as_deref()
                    .or(metadata.id.value_str.as_deref())
                    .expect("Only benchmarks with groups are supported, for now"),
            );
            assert_eq!(bench_directory, expected_bench_directory);

            let mut latest_datetime = None;
            for meas in bench
                .measurements()
                .expect("Failed to start reading measurements")
            {
                let meas = meas.expect("Failed to open a measurement file");
                let datetime = meas.datetime();
                println!("--- Loading measurement from {datetime} ---\n");

                match (datetime, latest_datetime) {
                    (t, None) => latest_datetime = Some(t),
                    (t, Some(l)) if t >= l => latest_datetime = Some(t),
                    _ => {}
                }

                let data = meas.data().expect("Failed to read measurement data");
                println!("{data:?}\n");

                assert!(
                    (data.datetime.date_naive() - datetime.date_naive()) < TimeDelta::minutes(1),
                    "Internal date doesn't match file date"
                );
                assert!(data.iterations.iter().all(|iter| *iter == iter.trunc()));
                assert_eq!(data.iterations.len(), data.values.len());
                assert_eq!(data.iterations.len(), data.avg_values.len());
                assert!(data.avg_values.iter().copied().eq(data
                    .iterations
                    .iter()
                    .zip(&data.values)
                    .map(|(&iters, &value)| value / iters)));
                assert_eq!(data.throughput, metadata.id.throughput);
                assert_eq!(data.changes.is_some(), data.change_direction.is_some());
            }

            assert_eq!(
                latest_datetime,
                Some(metadata.latest_datetime()),
                "Latest date/time in benchmark.cbor doesn't match latest file date/time"
            );
        }
    }
}
