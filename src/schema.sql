-- ### Definition of the SQLite schema used by criterion_cbor ###

-- One row per Criterion benchmark with measurements
CREATE TABLE benchmark(
    -- Relative path to the benchmark data files below the `cargo criterion`
    -- data root <cargo root>/target/criterion/main. This is a good choice of
    -- primary key because its uniqueness is enforced by the OS filesystem.
    relative_path TEXT NOT NULL
                  PRIMARY KEY,

    -- Last modification timestamp of the benchmark.cbor file at the time where
    -- this entry was created.
    modified TEXT NOT NULL
             UNIQUE
             CHECK (
                 date_time GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].*Z'
             ),

    -- The rest is the metadata from the benchmark.cbor file, except...
    -- * id.throughput is not included because if correct it can be generated
    --   from measurement throughputs, see the `benchmark_throughput` view.
    -- * latest_record is not included because if correct it can be generated
    --   from measurement date/times, see the `latest_measurement` view.

    -- This metadata is called `group_id` in CBOR, but actually it can hold
    -- function names if the `Criterion::bench_xyz` functions are used without
    -- creating a benchmark group first.
    group_or_function TEXT NOT NULL,

    -- This metadata is called `function_id` in CBOR, but it is only filled when
    -- running benchmarks that have a function name (not just a
    -- `BenchmarkId::from_parameter`) inside of a benchmark group.
    function_in_group TEXT,

    -- This metadata is generated from parameter data by the criterion
    -- `BenchmarkId::new()`/`from_parameter()` mechanism.
    value_str TEXT
) STRICT, WITHOUT ROWID;


-- One row per mean/median/std_dev/... estimate from a measurement
--
-- Estimates are derived from samples, so they are invalidated if the set of
-- samples from a measurement changes. Unfortunately, we cannot enforce this set
-- of constraints in SQLite as its CHECK constraints and triggers are too
-- limited to allow for this.
CREATE TABLE estimate(
    -- Auto-filled via SQLite's ROWID mechanism, see the SQLite docs to learn
    -- more about it and why it's better than AUTOINCREMENT in this situation.
    --
    -- We need to use a technical identifier as a primary key here because
    -- estimates are identified by a source table and field and there is no
    -- clean mapping of these notions to the SQLite abstraction vocabulary.
    id INTEGER NOT NULL
               PRIMARY KEY,

    -- Most likely value of the statistic of interest
    --
    -- Check the definition of the foreign key that pointed you to this estimate
    -- row in order to find out what statistic we're talking about.
    point_estimate REAL NOT NULL,

    -- Standard error of the statistic exposed by `point_estimate`
    standard_error REAL NOT NULL
                   CHECK (standard_error >= 0.0),

    -- Confidence interval range (usually 0.95 aka a 95% confidence interval)
    confidence_level REAL NOT NULL
                     CHECK (confidence_level BETWEEN 0.0 AND 1.0)
                     DEFAULT 0.95,

    -- Lower bound of the confidence interval
    lower_bound REAL NOT NULL
                CHECK (lower_bound <= point_estimate),

    -- Upper bound of the confidence interval
    upper_bound REAL NOT NULL
                CHECK (upper_bound >= point_estimate)
) STRICT;


-- One row per measurement of a given Criterion benchmark
CREATE TABLE measurement(
    -- Benchmark workload that was measured
    relative_path TEXT NOT NULL
                  REFERENCES benchmark
                             ON DELETE CASCADE
                             ON UPDATE CASCADE,

    -- YYMMDDHHMMSS local date/time suffix of the data file
    --
    -- We need this to find the data file again in the future on since it is not
    -- a perfect derivative of `date_time` due to complications like Daylight
    -- Saving Time or system timezone setting changes.
    file_id TEXT NOT NULL
            CHECK (
                file_id GLOB '[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]'
            ),

    -- UTC date/time at which the measurement was saved, in ISO-8601 format
    --
    -- Among the formats that SQLite supports, we picked this one because...
    -- * It's self-describing, every programmer that ever dealt with textual
    --   dates and times will recognize it at first sight.
    -- * Unlike Julian day numbers, it is not at risk of being encoded into
    --   a floating-point number with a loss in precision.
    -- * Unlike Unix timestamps, it is supported by all SQLite date/time
    --   functions without special flags.
    date_time TEXT NOT NULL
              UNIQUE
              CHECK (
                  date_time GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].*Z'
              ),

    -- ### Statistical estimates derived from measurement samples ###

    -- Mean of `sample.average`
    mean INTEGER NOT NULL
         UNIQUE
         REFERENCES estimate(id)
                    ON UPDATE CASCADE,
    -- Median value of `sample.average`
    median INTEGER NOT NULL
           UNIQUE
           REFERENCES estimate(id)
                      ON UPDATE CASCADE,
    -- Slope of the linear regression of sample `value` against `iterations`.
    -- Will only be present if linear sampling was used.
    slope INTEGER
          UNIQUE
          REFERENCES estimate(id)
                     ON UPDATE CASCADE,
    -- Standard deviation of `sample.average`
    std_dev INTEGER NOT NULL
            UNIQUE
            REFERENCES estimate(id)
                       ON UPDATE CASCADE,
    -- Median absolute deviation of `sample.average`
    median_abs_dev INTEGER NOT NULL
                   UNIQUE
                   REFERENCES estimate(id)
                              ON UPDATE CASCADE,

    -- ### User-provided optional historical context about the measurement ###

    -- An optional identifier string such as a commit ID that will be shown in
    -- the history reports to identify this run.
    history_id TEXT,

    -- An optional description string such as a commit message that will be
    -- shown in the history reports to describe this run.
    history_desc TEXT,

    -- Best primary key because uniqueness is enforced by OS filesystem
    PRIMARY KEY (relative_path, file_id)
) STRICT, WITHOUT ROWID;
--
-- Emulate ON DELETE CASCADE for estimates associated with a measurement, so
-- that deleting a measurement deletes the associated estimates.
CREATE TRIGGER measurement_delete_estimates
       AFTER DELETE ON measurement
       FOR EACH ROW
       BEGIN
           DELETE FROM estimate
           WHERE estimate.id IN (
               OLD.mean,
               OLD.median,
               OLD.median_abs_dev,
               OLD.slope,
               OLD.std_dev
           );
       END;
--
-- Indices for each foreign key
CREATE INDEX measurement_relative_path ON measurement(relative_path);
CREATE UNIQUE INDEX measurement_mean ON measurement(mean);
CREATE UNIQUE INDEX measurement_median ON measurement(median);
CREATE UNIQUE INDEX measurement_slope ON measurement(slope);
CREATE UNIQUE INDEX measurement_std_dev ON measurement(std_dev);
CREATE UNIQUE INDEX measurement_median_abs_dev ON measurement(median_abs_dev);


-- Optional throughput specification for a measurement
CREATE TABLE throughput(
    -- Measurement that this throughput specification is associated with
    relative_path TEXT NOT NULL,
    file_id TEXT NOT NULL,

    -- Number of `unit`s that were processed during the measurement
    amount INTEGER NOT NULL
           CHECK (amount > 0),
    -- BytesDecimal is like Bytes but formatted differently when displayed
    unit TEXT NOT NULL
         CHECK (unit IN ('Bytes', 'BytesDecimal', 'Elements')),

    -- Throughput specifications are optional measurement metadata
    PRIMARY KEY (relative_path, file_id),
    FOREIGN KEY (relative_path, file_id) REFERENCES measurement
                                                    ON DELETE CASCADE
                                                    ON UPDATE CASCADE
) STRICT, WITHOUT ROWID;
--
-- Index for the foreign key
CREATE UNIQUE INDEX throughput_measurement ON throughput(relative_path, file_id);
-- Index for selection key of `benchmark_throughput` view
CREATE INDEX throughput_relative_path ON throughput(relative_path);


-- One row per data sample from a given measurement
CREATE TABLE sample(
    -- Measurement during which this data sample was measured
    relative_path TEXT NOT NULL,
    file_id TEXT NOT NULL,

    -- Index of the sample in the iteration/value tables of the CBOR data file
    idx INTEGER NOT NULL
        CHECK (
            -- We'd also like to enforce that predecessors of idx exist, i.e. if
            -- idx > 0 then idx - 1 exists, but...
            -- * We can't enforce this property via CHECK constraints because
            --   subqueries aren't allowed in CHECK constraints and we need them
            --   to check out the value of other rows.
            -- * We can't enforce this property via triggers because SQLite only
            --   supports FOR EACH ROW triggers and here we need a FOR EACH
            --   STATEMENT trigger to check the final table configuration after
            --   all pending idx insertions/deletions have been processed.
            idx >= 0
        ),

    -- Number of function iterations that were executed
    iterations INTEGER NOT NULL
               CHECK (
                   -- Since the source data is f64, we will also need to make
                   -- sure that the source data is actually made of integers, on
                   -- the side of the code that reads out the CBOR data.
                   iterations >= 0
               ),

    -- Value that was measured (typically a wall-clock time in nanoseconds)
    value REAL NOT NULL,

    -- Average of the measured value across function iterations
    average REAL NOT NULL
            GENERATED ALWAYS AS (value / iterations),

    -- A measurement contains N different data samples
    PRIMARY KEY (relative_path, file_id, idx),
    FOREIGN KEY (relative_path, file_id) REFERENCES measurement
                                                    ON DELETE CASCADE
                                                    ON UPDATE CASCADE
) STRICT, WITHOUT ROWID;
--
-- Index for the foreign key
CREATE INDEX sample_measurement ON sample(relative_path, file_id);


-- One row per measurement for which a change with respect to the previous
-- measurement on the same benchmark has been estimated
--
-- As of today this should be true of all measurements except for the first one
-- recorded for a particular benchmark. But criterion may allow users to disable
-- the change detection someday, so I'm not encoding this in the schema...
CREATE TABLE change(
    -- Measurement from which this change estimate is taken
    relative_path TEXT NOT NULL,
    file_id TEXT NOT NULL,

    -- Outcome of the change direction analysis
    direction TEXT NOT NULL
              CHECK (
                  direction in ('NoChange', 'NotSignificant', 'Improved', 'Regressed')
              ),

    -- Mean magnitude of the change across samples
    mean INTEGER NOT NULL
         REFERENCES estimate(id)
                    ON UPDATE CASCADE,

    -- Median magnitude of the change across samples
    median INTEGER NOT NULL
           REFERENCES estimate(id)
                      ON UPDATE CASCADE,

    -- A change estimate is an optional part of a measurement
    PRIMARY KEY (relative_path, file_id),
    FOREIGN KEY (relative_path, file_id) REFERENCES measurement
                                         ON DELETE CASCADE
                                         ON UPDATE CASCADE
) STRICT, WITHOUT ROWID;
--
-- Emulate ON DELETE CASCADE for estimates associated with a change, so that
-- deleting a change report deletes the associated change estimates.
CREATE TRIGGER change_delete_estimates
       AFTER DELETE ON change
       FOR EACH ROW
       BEGIN
           DELETE FROM estimate
           WHERE estimate.id IN (OLD.mean, OLD.median);
       END;
--
-- Indices for each foreign key
CREATE UNIQUE INDEX change_measurement ON change(relative_path, file_id);
CREATE UNIQUE INDEX change_mean ON change(mean);
CREATE UNIQUE INDEX change_median ON change(median);


-- Latest measurement from each benchmark
CREATE VIEW latest_measurement(relative_path, file_id) AS
    SELECT relative_path, file_id
    FROM measurement m
    WHERE date_time = (
        SELECT max(date_time)
        FROM measurement
        WHERE relative_path = m.relative_path
    );


-- Common throughput for all measurements in a benchmark
--
-- This is only available for benchmarks where all measurements were configured
-- with the same throughput. You would expect this to usually be true, but
-- criterion does not actually enforce it anywhere, so if you change the
-- throughput of a benchmark you potentially get inconsistent data...
CREATE VIEW benchmark_throughput(relative_path, amount, unit) AS
    SELECT DISTINCT relative_path, amount, unit
    FROM throughput t
    WHERE
        -- All measurements from this benchmark have a throughput
        (
            SELECT count(*)
            FROM throughput
            WHERE relative_path = t.relative_path
        ) = (
            SELECT count(*)
            FROM measurement
            WHERE relative_path = t.relative_path
        )
        AND
        -- Configured throughputs are the same for all measurements
        NOT EXISTS (
            SELECT *
            FROM throughput
            WHERE relative_path = t.relative_path
              AND ((amount != t.amount) OR (unit != t.unit))
        );
