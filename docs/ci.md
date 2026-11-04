# CI

Two workflows split the checking. Everything that runs by itself answers "is
it correct?"; the profile, run only when someone asks for it, answers "is it
still fast?". A shared GitHub runner can't answer the second question
reliably: it reads up to 4× over PRD §6's budgets and varies 2× from one run
to the next, and a busy one can stall a thread for seconds. So no automatic
run judges speed, and speed never blocks a merge or a release.

## Automatically: every pull request and push to `main`

[`ci.yml`](../.github/workflows/ci.yml): correctness only. A release doesn't
run it again: [`release.yml`](../.github/workflows/release.yml) builds at once
but publishes only once CI has passed on `main` for the tagged commit.
Each job has a `timeout-minutes` well past a normal run, so a hang fails the
job instead of running for GitHub's six-hour default.

| Job | What it runs |
| --- | --- |
| Format and clippy | `cargo fmt --all --check`, then clippy with `-D warnings` |
| Build, test and bench (macOS, Windows, Ubuntu); the name is from before the benchmark moved, kept so a ruleset's required checks by it still report | On Ubuntu, frees disk space first; `cargo build --workspace --release`, then `cargo test --workspace --no-fail-fast`; on macOS, `mudling-desktop --synthetic --ax-check` |
| iOS core build | The XCFramework and Swift bindings, then a Swift type-check |

`--no-fail-fast` keeps one failing test binary from hiding the failures in the
others. No benchmark runs, nothing is compared, and no test that judges speed
runs: those are ignored here.

## On demand: the profile

[`profile.yml`](../.github/workflows/profile.yml), only when started by hand:
Actions, **Profile**, **Run workflow**, on any branch. It has no schedule, and
no push or pull request starts it.

| Job | What it does | What fails it |
| --- | --- | --- |
| Replay benchmark (three runners) | `mudling-bench --json`, on the committed synthetic fixture; uploads `bench-report-<os>` | Only the benchmark failing to run |
| Benchmark against the last measured commit | `mudling-bench compare` of the profiled commit against the commit the previous profile of `main` measured, or the latest `v*` release when there is none; uploads `bench-compare` | A metric more than 10% worse on both comparisons (it retries once) |
| Timing tests (three runners) | `cargo test --workspace --no-fail-fast -- --ignored timing_`; uploads `timing-tests-<os>` | A test that fails all three attempts |

A comparison runs both builds in turns on one machine, so a slow spell hits
both alike. The timing tests run up to three times, and a retry reruns only
the tests that failed. Open the run to see which job failed; the reports and
test logs are its artifacts.

## What a test may depend on

A test in the automatic run must pass however slow or busy the machine is. So
it never asserts on how fast something ran, and it orders work between
threads with a channel, a flush, a watch or a store it waits on, not a sleep.
A wait for another thread may end in a hang guard, of 10 seconds or more,
that a passing test never reaches; most use 60. A limit under test starts only
once the work has begun, or ends on a signal. A test that only shows that
something is still waiting may use a short timeout, as a slow machine can only
make it wait longer.

## Timing tests

A test whose pass or fail depends on how fast the machine is carries
`#[ignore = "timing: runs in the profile"]` and a name starting `timing_`. The
prefix is what the profile selects by: `-- --ignored` alone would also run the
tests ignored for other reasons (a GPU, the live registry, child processes).

Where a test checks behaviour and also a time, the behaviour is its own test
and stays in the automatic run; the timing test shares its body through a
helper that returns the elapsed time.

Run them locally, on a quiet machine:

```sh
cargo test --workspace -- --ignored timing_
```

Always use the `timing_` filter. An unfiltered `cargo test -- --ignored` also
runs tests that rewrite the committed fixture
`crates/mudling-core/src/vault/testdata/format-1.vault`, and tests that reach
the GPU (image files) and the live registry.

## Holding the budgets locally

`mudling-bench --check` exits non-zero when a PRD §6 budget is missed. Run it
before each phase's pull request, on a quiet machine: close other builds, the
browser and anything that polls, and let the laptop be on power. Trust a miss
only after a rerun on that machine; a number from a busy one tells you about
the busy machine.

```sh
cargo run --release -p mudling-bench -- --check
```
