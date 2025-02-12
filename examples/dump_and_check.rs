//! This utility dumps the data recorded by criterion inside of a certain
//! project, while also checking the invariants that criterion-cbor makes about
//! the behavior of cargo-criterion.

use chrono::{DateTime, Local, TimeDelta};
use criterion_cbor::Search;
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
    for bench in Search::in_cargo_root(cargo_root).find_all() {
        let bench = bench.expect("Failed to open a benchmark's directory");
        let bench_path = bench.path_from_data_root();
        println!(
            "\n=== Loading benchmark data from path {} ===\n",
            bench_path.display()
        );

        let metadata = bench.metadata().expect("Failed to read benchmark metadata");
        println!("id: {:#?}", metadata.id.decode());
        println!(
            "latest_local_datetime: {:#?}\n",
            metadata.latest_local_datetime()
        );

        let make_filename_safe = |s: &str| {
            s.replace(
                &['?', '"', '/', '\\', '*', '<', '>', ':', '|', '^'][..],
                "_",
            )
        };
        let mut expected_path =
            PathBuf::from(make_filename_safe(&metadata.id.group_or_function_id));
        if let Some(function_id) = &metadata.id.function_id_in_group {
            expected_path.push(make_filename_safe(function_id));
        }
        if let Some(value_str) = &metadata.id.value_str {
            expected_path.push(make_filename_safe(value_str));
        }
        assert_eq!(bench_path, expected_path);

        let mut latest_datetime = None;
        for meas in bench.measurements() {
            let datetime = meas.local_datetime();
            println!("--- Loading measurement from time {datetime:?} ---\n");

            match (datetime, latest_datetime) {
                (t, None) => latest_datetime = Some(t),
                (t, Some(l)) if t.earliest() >= l.earliest() => latest_datetime = Some(t),
                _ => {}
            }

            let data = meas.data().expect("Failed to read measurement data");
            println!("{data:?}\n");

            let date_close_to = |date: Option<DateTime<Local>>| {
                date.map_or(false, |date| {
                    data.datetime.date_naive() - date.date_naive() < TimeDelta::minutes(1)
                })
            };
            assert!(
                date_close_to(datetime.earliest()) || date_close_to(datetime.latest()),
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
            Some(metadata.latest_local_datetime()),
            "Latest date/time in benchmark.cbor doesn't match latest file date/time"
        );
    }
}
