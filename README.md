<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/brand/mudling-primary-dark.png">
    <img src="assets/brand/mudling-primary-light.png" width="320" alt="Mudling">
  </picture>
</p>

**MUDling** is a touch-first, open-source MUD client. A mudling is a small
creature of living mud — and MUD is where it lives: exits become a D-pad,
the things in the room become tappable, and commands compose by touch
instead of typing. Protocol-aware (GMCP/MXP) and accessibility-first.

> Early days. The headless core is here: the connection layer (telnet, GMCP,
> MCCP2), line pipeline, plugin host with Luau hooks, versioned stores, map,
> persistence and completion, with the Avatar game plugin on top. The desktop
> shell plays on it: a gpui window that opens saved servers, with a chat you
> can search and read back through, a Room panel with a D-pad, a hotbar of
> commands, the character's HUD, and settings for plugins, triggers, aliases
> and shortcuts. It is built for the keyboard and screen readers as well as
> the mouse. [docs/phase-0-spike.md](docs/phase-0-spike.md) records the spike
> it grew from. The iOS app is still to come.

## Building

MUDling is a Cargo workspace:

| Path | What it is |
|---|---|
| `crates/mudling-core` | Everything except drawing: connection, line pipeline, plugins, stores, map, persistence, completion, scrollback |
| `crates/mudling-desktop` | The desktop shell, drawn with [gpui](https://gpui.rs) |
| `crates/mudling-bench` | The headless benchmarks against the performance budgets |
| `crates/mudling-ffi` | UniFFI bindings for the iOS app: store subscriptions and diffs (commands only echo for now) |
| `crates/mudling-cli` | The `mudling` command line, for plugin authors: `mudling plugin new` and `mudling plugin test` |
| `plugins/avatar/` | The first game plugin, in TOML and Luau |
| `plugins/aardwolf/` | The Aardwolf game plugin: TOML alone, over Aardwolf's GMCP |
| `docs/plugins/` | The plugin [API reference](docs/plugins/reference.md) and [cookbook](docs/plugins/cookbook.md) |
| `apple/` | The iOS app, to come |

```sh
cargo build --release
cargo test
cargo run --release -p mudling-bench                 # the core's budgets (--check exits 1 on a miss)
cargo run --release -p mudling-desktop -- --synthetic # the chat, with made-up lines
```

On Debian or Ubuntu, `scripts/install-linux-deps.sh` installs the libraries
gpui needs.

Release builds carry line-table debug info, so daily builds are release
builds that still give useful backtraces.

**Benchmarks.** `mudling-bench --check` holds the core to PRD §6's numbers. Run
it locally, on a quiet machine, before each phase's pull request. CI runs the
benchmark and keeps its report, but gates none of the numbers: GitHub's shared
runners read up to 4× over them and vary 2× from run to run. What CI does gate
is `mudling-bench compare`, which runs a pull request's build and its base's in
turns on one machine and fails a loss of more than 10%.

**gpui is pinned** to `gpui-pre 0.3.10`, an exact published snapshot of Zed's
`main` (zed@08cbb88). The crates.io `gpui` release (0.2.2) predates gpui's
AccessKit support, which MUDling needs for screen readers. Upgrades are
deliberate; [docs/phase-0-spike.md](docs/phase-0-spike.md#why-this-gpui)
explains the choice.

---

*A [Vyttle](https://vyttle.com) project.*
