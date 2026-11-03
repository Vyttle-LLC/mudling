# CI

Two workflows split the checking. A pull request answers "is it correct?"; a
daily run on `main` answers "is it still fast?". A shared GitHub runner can't
answer the second question reliably: it reads up to 4× over PRD §6's budgets
and varies 2× from one run to the next. Speed never blocks a merge.

## On every pull request and push to `main`

[`ci.yml`](../.github/workflows/ci.yml). The release workflow runs the same
workflow first, so a tag only builds from a correct `main`.

| Job | What it runs |
| --- | --- |
| Format and clippy | `cargo fmt --all --check`, then clippy with `-D warnings` |
| Build, test and bench (macOS, Windows, Ubuntu); the name is from before the benchmark moved, kept so a ruleset's required checks by it still report | `cargo build --workspace --release`, then `cargo test --workspace --no-fail-fast`; on macOS, `mudling-desktop --synthetic --ax-check` |
| iOS core build | The XCFramework and Swift bindings, then a Swift type-check |

`--no-fail-fast` keeps one failing test binary from hiding the failures in the
others. No benchmark runs, nothing is compared, and no test that judges speed
runs: those are ignored here.

## Daily, on `main`

[`daily.yml`](../.github/workflows/daily.yml), at 06:17 UTC and on demand
(Actions, **Daily performance**, **Run workflow**, on `main`).

| Job | What it does | What fails it |
| --- | --- | --- |
| Replay benchmark (three runners) | `mudling-bench --json`, on the committed synthetic fixture; uploads `bench-report-<os>` | Only the benchmark failing to run |
| Benchmark against the last measured commit | `mudling-bench compare` of `main`'s tip against the commit the previous daily run measured, or the latest `v*` release when there is none; uploads `bench-compare` | A metric more than 10% worse on both comparisons (it retries once) |
| Timing tests (three runners) | `cargo test --workspace --no-fail-fast -- --ignored timing_`; uploads `timing-tests-<os>` | A test that fails all three attempts |

A comparison runs both builds in turns on one machine, so a slow spell hits
both alike. The timing tests run up to three times, and a retry reruns only
the tests that failed.

A failed job turns the run red. GitHub sends its own failure notice for a
scheduled workflow, to whoever last changed the schedule and to those who watch
the repository's Actions. Open the run to see which job failed; the reports
and test logs are its artifacts.

## Timing tests

A test whose pass or fail depends on how fast the machine is carries
`#[ignore = "timing: runs daily"]` and a name starting `timing_`. The prefix is
what the daily run selects by: `-- --ignored` alone would also run the tests
ignored for other reasons (a GPU, the live registry, child processes).

Where a test checks behaviour and also a time, the behaviour is its own test
and stays in pull request CI; the timing test shares its body through a helper
that returns the elapsed time.

Run them locally, on a quiet machine:

```sh
cargo test --workspace -- --ignored timing_
```

`cargo test -- --ignored` with no filter runs the other ignored tests too.

## Holding the budgets locally

`mudling-bench --check` exits non-zero when a PRD §6 budget is missed. Run it
before each phase's pull request, on a quiet machine: close other builds, the
browser and anything that polls, and let the laptop be on power. Trust a miss
only after a rerun on that machine; a number from a busy one tells you about
the busy machine.

```sh
cargo run --release -p mudling-bench -- --check
```
