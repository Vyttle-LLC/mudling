# Plugin API reference

This is API v1, the one MUDling implements today. The
[cookbook](cookbook.md) works through a real plugin; this page lists what
there is. A plugin is a folder of TOML with, where patterns aren't enough, a
few Luau hooks. Nothing here needs you to read the core.

`crates/mudling-cli/tests/docs.rs` checks this page against the code. Each
table of keys must list exactly the keys the loader accepts, and the `event`
and `mud` tables exactly what a hook sees, so a change to the API fails that
test until this page changes with it.

## Quick start

```sh
cargo build --release -p mudling-cli
target/release/mudling plugin new mygame
target/release/mudling plugin test mygame
```

That makes a folder that loads and passes its own test. Edit its files, then
run `plugin test` again. See [Commands](#commands).

## The plugin folder

```text
mygame/
  plugin.toml        the manifest
  text.toml          one TOML file for each extension point you use
  hooks.luau         Luau hooks, if any
  tests/sample.txt   a small log of made-up text
  tests/sample.expected
```

Every file the manifest names must be inside the folder. Loading checks
everything, every pattern included, and rejects the plugin at the first
problem, naming the file and line. A plugin that loads can't fail later because
of a bad pattern.

## `plugin.toml`

<!-- keys: plugin.toml root -->
| Key | Required | What it is |
| --- | --- | --- |
| `id` | yes | Lowercase letters, digits, `.`, `_` and `-`, starting with a letter or digit. |
| `name` | yes | The name players see. |
| `version` | yes | `MAJOR.MINOR.PATCH`, such as `"1.0.0"`. |
| `api` | yes | The API version: `1`. |
| `license` | yes | Registry plugins must be MIT or Apache-2.0. |
| `layer` | yes | `"default"`, `"game"` or `"override"`. See [Layers](#layers). |
| `encoding` | no | `"utf-8"` or `"latin-1"`. Only one applies: the highest layer's. |
| `servers` | for `game` and `override` | The `[servers]` table. A `default` plugin can't have one. |
| `points` | no | The `[points]` table. |
| `permissions` | no | What hooks may do. See [Permissions](#permissions). |
| `hooks` | no | The `[hooks]` table. |
| `fragments` | no | The `[fragments]` table: regex shared by this plugin's patterns. See [Patterns](#patterns). |

<!-- keys: plugin.toml [servers] -->
| `[servers]` key | What it is |
| --- | --- |
| `hosts` | Exact hosts, or one leading wildcard: `"mud.example.org"`, `"*.example.org"`. Case doesn't matter. |
| `mssp_names` | The server's MSSP `NAME`, exactly. Case doesn't matter. |

<!-- keys: plugin.toml [points] -->
| `[points]` key | The file that declares it |
| --- | --- |
| `connection` | [Connection](#connection) |
| `commands` | [Commands point](#commands-point) |
| `text` | [Text](#text) |
| `hud` | [HUD](#hud) |
| `map` | [Map](#map) |
| `character` | [Character](#character) |
| `gear` | [Gear](#gear) |
| `mobs` | [Mobs](#mobs) |
| `ui` | [UI](#ui) |

<!-- keys: plugin.toml [hooks] -->
| `[hooks]` key | What it is |
| --- | --- |
| `files` | The Luau files, loaded in this order into one VM. |

## Patterns

A pattern is a regex in the syntax of Rust's
[`regex`](https://docs.rs/regex/latest/regex/#syntax) crate, matched against a
line's plain text, with colors stripped. It runs in linear time, so no pattern
can hang the client. There are no lookarounds or backreferences.

Write regexes in `'''literal'''` strings, so nothing needs escaping twice, and
give each a comment showing the line it reads. Named captures, `(?<name>…)`,
become the event's data.

A **fragment** is regex shared by a plugin's patterns, written `{name}`, and
goes in as a group, so `{prompt}?` makes all of it optional. `\{` is a literal
brace, and a `{` that names no fragment is read by the regex, so `{2,}` is
still a repeat. A fragment's name uses letters, digits and `_`, and doesn't
start with a digit.

## Events

An **event rule** is a pattern that emits a named event when a line matches,
with its named captures as data. All the lists marked "event rules" below take
these keys:

<!-- keys: text.toml [[pattern]] -->
| Key | Required | What it is |
| --- | --- | --- |
| `name` | yes | The event's name: letters, digits, `_`, `.` and `-`. Rules may share one. |
| `pattern` | yes | The regex. |
| `hook` | no | The Luau function that gets the event. |

A rule's name can carry meaning of its own:

| Name | What it does |
| --- | --- |
| `<reader>.stale` | Asks for that reader's background command again when the line appears. One rule per reader, each with the same pattern if one line makes several stale. Needs no hook. |
| `<reader>.none` | Marks a one-line answer to a background command, with no table. That line and the prompt after it stay out of the chat. |
| `*.did_not_fit` | In Gear's `put`: the container refused the item while a gear set went on. |
| `*.not_carried` | In Gear's `put`: the item hadn't come off yet while a gear set went on. |

Each event goes to its hook, if it names one, and to whatever in the core reads
that point. Hooks run only on lines their pattern matched.

## Tables

A **table reader** frames a table the game prints, so a hook sees it as a run
of events:

<!-- keys: character.toml [[reader]] -->
| Key | Required | What it is |
| --- | --- | --- |
| `name` | yes | The reader's name, the start of its events' names. |
| `start` | yes | A pattern or a list of them for the line that opens the table. |
| `row` | yes | A pattern or a list of them. Rows match only while the table is open. |
| `end` | no | The pattern for the line that closes it. |
| `hook` | no | The Luau function that gets all of its events. |

| Event | When |
| --- | --- |
| `<name>.start` | A line that opens it. One more start before any row is the same table, such as a header under its title. |
| `<name>.row` | A row. |
| `<name>.gap` | The first line after a row, or after the start, that's no row. |
| `<name>.end` | Its `end` pattern, any prompt, or the plugin's next table. |
| `<name>.cut` | It ended at a pager prompt no one answered, or with the connection, so what was read isn't all of it. |

A plugin reads one table at a time. Row patterns cost nothing outside a table.

## The extension points

Each point is a TOML file named in `[points]`. Use only those you need.

### Connection

<!-- keys: connection.toml root -->
| Key | What it is |
| --- | --- |
| `gmcp` | GMCP modules to subscribe to, such as `"Char 1"`, beside the core's own: `Core 1`, `Char 1` and `Room 1`. |
| `login` | The `[[login]]` list. |

<!-- keys: connection.toml [[login]] -->
| `[[login]]` key | What it is |
| --- | --- |
| `pattern` | The prompt. |
| `send` | `"name"`, `"password"` or `"blank"`, an empty line to get past a banner. |

Each prompt is answered once, because a server that asks for the password twice
has refused it.

### Commands point

<!-- keys: commands.toml root -->
| Key | What it is |
| --- | --- |
| `separator` | What joins chained commands on one line. With none, a chain goes as separate lines. Only one applies: the highest layer's. |
| `verbs` | A table of the game's word for each verb: `kill = "slay"`. MUDling asks for `examine`, `get`, `hold`, `kill`, `look`, `practice`, `put`, `remove`, `wear` and `wield`, and uses the key itself when a game doesn't list one. |
| `doors` | A table of door commands, `{dir}` standing for the direction. Only `open` is used: see [Declared, not used yet](#declared-not-used-yet). |
| `speedwalk` | The `[speedwalk]` table: the game's own syntax. With none, a walk expands in the client, one move a line. |
| `alias` | The `[[alias]]` list. A player's alias beats a plugin's. |
| `keywords` | The `[keywords]` table: which word of an item's name a command should use. Gear sets use it, and so does `mud.keyword`. |

<!-- keys: commands.toml [speedwalk] -->
| `[speedwalk]` key | What it is |
| --- | --- |
| `step` | One run of steps, with `{count}` and `{dir}`: `"{count}{dir}"`. |
| `join` | What goes between runs. |

<!-- keys: commands.toml [[alias]] -->
| `[[alias]]` key | What it is |
| --- | --- |
| `name` | What the player types. |
| `expansion` | What it stands for. |

<!-- keys: commands.toml [keywords] -->
| `[keywords]` key | What it is |
| --- | --- |
| `skip` | Words that are never the keyword, such as articles. |
| `phrase_ends` | Words that end the phrase holding the keyword: "of" in "a bottle of water". |
| `possessives` | Whether the game takes a possessive; defaults to `true`. If not, one is never chosen while there's another word. |

### Text

<!-- keys: text.toml root -->
| Key | What it is |
| --- | --- |
| `pager` | The `[pager]` table. Only one applies: the highest layer's. |
| `prompt` | The `[[prompt]]` list: the prompt's shape, for servers that send no GA or EOR. |
| `pattern` | Event rules over each finished line: trigger packs, and anything no other point reads. |
| `color` | The `[[color]]` list: coloring for lines the server sends without any. |
| `link` | The `[[link]]` list: links and actions in lines. |

<!-- keys: text.toml [pager] -->
| `[pager]` key | What it is |
| --- | --- |
| `prompt` | The pager's prompt, which arrives without a line end. |
| `key` | What to send to continue; empty sends a blank line. The client sends it while a table is being read. |

<!-- keys: text.toml [[prompt]] -->
| `[[prompt]]` key | What it is |
| --- | --- |
| `pattern` | Text the server hasn't ended is a prompt if this covers all of it. A finished line starts with a prompt if this matches at its start, so a prompt ends a table even with output glued after it. |

<!-- keys: text.toml [[color]] -->
| `[[color]]` key | What it is |
| --- | --- |
| `pattern` | What to color. |
| `fg`, `bg` | Palette indexes. |
| `bold` | `true` or `false`. A rule must set at least one of `fg`, `bg` and `bold`. |
| `group` | Colors only this named capture, not the whole match. |

<!-- keys: text.toml [[link]] -->
| `[[link]]` key | What it is |
| --- | --- |
| `pattern` | What to link. |
| `action` | A command to send or a URL to open, each `{capture}` standing for its text: `"examine {item}"`. |
| `group` | Links only this named capture, not the whole match. |

### HUD

MUDling learns the common vitals from the first `Char.Vitals` by itself. These
are for a game that doesn't use the common names.

<!-- keys: hud.toml root -->
| Key | What it is |
| --- | --- |
| `vitals` | A table of each vital and the GMCP field that holds it: `hp = "Char.Vitals.hp"`. |
| `opponent` | The opponent bar's fields, the same way. See [Declared, not used yet](#declared-not-used-yet). |
| `affects` | The GMCP field that lists affects: `"Char.Status.affects"`. It should hold an object of each affect and how long it lasts. |

### Map

<!-- keys: map.toml root -->
| Key | What it is |
| --- | --- |
| `room_identity` | `"gmcp"`, the room number, the default; or `"description"`: name, exits and description. Only one applies: the highest layer's. |
| `areas` | A table of area names by the game's zone id. |
| `area_name` | A pattern reading `Room.Info`'s zone where `areas` doesn't name it: the area as `name`, its levels as `low` and `high`. Only one applies: the highest layer's. |
| `exits` | Event rules: the line lists the room's exits, and its description follows. |
| `failed_move` | Event rules: the game refused a move. |
| `darkness` | Event rules: the room is too dark to see. |
| `flee` | Event rules: a flee, the way captured as `dir`. |
| `maze` | The `[[maze]]` list: rooms whose exits don't hold still. |

<!-- keys: map.toml [[maze]] -->
| `[[maze]]` key | What it is |
| --- | --- |
| `area` | The area. |
| `rooms` | The rooms' names. With none listed, the whole area. |

### Character

<!-- keys: character.toml root -->
| Key | What it is |
| --- | --- |
| `command` | The `[[command]]` list: commands the client sends quietly. |
| `reader` | The `[[reader]]` list of [table readers](#tables). |
| `skill_improved` | Event rules: a message that a skill improved. |

<!-- keys: character.toml [[command]] -->
| `[[command]]` key | What it is |
| --- | --- |
| `command` | What to send. |
| `reader` | The reader of its reply, from this point or Gear. |

A background command goes out when the character enters the game, when a rule
named `<reader>.stale` matches, and, for Gear's table, when gear goes on or
comes off. The Refresh button asks for every command whose reader is the
Character point's. The client waits for quiet: the player idle and out of a
fight, with a pause between reads.

Its reply stays out of the chat line by line, so the reader's `start`, `row`
and `end` patterns must match every line that should hide, ruled lines and
headings too. Any other line shows and fires triggers, as text the game sent
meanwhile should. A hook can ignore a row that holds no facts, but it should
do so cheaply: every row counts against its budget.

### Gear

<!-- keys: gear.toml root -->
| Key | What it is |
| --- | --- |
| `identify`, `examine`, `equipment` | Each a [table reader](#tables) as a table of its own: `[identify]`. `equipment` is the worn-gear table. |
| `slots` | Wear slots, in the order the game lists them. |
| `wear`, `remove` | Event rules. The core measures what an item adds from the stats around these, so give each an `item` capture. They also ask for Gear's background command again. |
| `put` | Event rules. While a gear set goes on, a rule with an `item` capture means it went in. |
| `containers` | Item keywords that are containers. See [Declared, not used yet](#declared-not-used-yet). |

### Mobs

<!-- keys: mobs.toml root -->
| Key | What it is |
| --- | --- |
| `aggro` | Event rules: a mob attacks on sight, captured as `name`. |
| `room_title` | Rules that turn a room title into a mob name, captured as `name`. They read titles, not lines, so they can't name a hook. The highest layer's rule that matches wins. |

### UI

<!-- keys: ui.toml root -->
| Key | What it is |
| --- | --- |
| `panels`, `character_fields` | See [Declared, not used yet](#declared-not-used-yet). |

## Declared, not used yet

API v1 froze these fields before anything read them. They load and are checked,
but MUDling does nothing with them yet, so leave them out unless you're
preparing for a later release:

| Field | Status |
| --- | --- |
| `ui.toml`'s `panels` and `character_fields` | Nothing reads them, so the point changes nothing. |
| `hud.toml`'s `opponent` | Nothing reads it. The opponent bar is filled from GMCP without it. |
| `gear.toml`'s `containers` | Nothing reads it. |
| `commands.toml`'s `doors`, other than `open` | `close`, `unlock` and `lock` are kept but nothing sends them. |

The API can't yet read a line about a locked door: the map has no rule for it.

## Hooks

Patterns are the main path. Write a hook for what they can't read, such as a
table cut into columns by width. List the files under `[hooks]`, and name a
global function as a rule's or reader's `hook`:

```lua
function on_welcome(event)
	mud.store("character", "name", event.captures.name)
end
```

Each hook gets one argument:

<!-- keys: hook event -->
| `event` field | What it is |
| --- | --- |
| `name` | The event's name. |
| `point` | The point's name, such as `"text"`. |
| `captures` | A table of each named capture that took part in the match. |
| `plain` | The line without colors. |
| `raw` | The line as it came. |

The `mud` table is the whole API:

<!-- keys: hook mud -->
| Call | Permission | What it does |
| --- | --- | --- |
| `mud.state(key)` | `read_state` | Reads a value, or `nil`. See [State](#state). |
| `mud.store(store, key, value)` | `write_store:<store>` | Writes a boolean, number or string to a named store. |
| `mud.send(command)` | `send_commands` | Sends a command, shown in the chat like the player's own. Returns `false` when over the rate limit. |
| `mud.refresh_inventory()` | `read_state` | Asks the server for the inventory again, for a game that doesn't report every change. Shares `mud.send`'s rate limit, and returns `false` when over it. |
| `mud.keyword(name)` | none | The word a command should use for an item, by the plugin's `[keywords]`. |

Without its permission, a call raises an error. A hook's outputs go out only
when it returns: one that fails or overruns sends nothing. What a file does at
its top level, as it loads, goes nowhere.

### Permissions

| Permission | What it allows |
| --- | --- |
| `read_state` | `mud.state` and `mud.refresh_inventory`. |
| `write_store:<name>` | `mud.store` to that store. |
| `send_commands` | `mud.send`. The plugin's hooks don't run until it's approved. Today only the desktop app's `--enable` approves a plugin: one turned on in Settings stays loaded with its hooks off until then. |

A plugin with no hooks needs no permissions.

### State

| Key | Value |
| --- | --- |
| `vitals.<name>` | A vital by its common name, such as `vitals.hp`. |
| `room.id`, `room.name`, `room.zone` | The current room. |
| `<store>.<key>` | Whatever a plugin wrote. |

### Stores

A store's name and keys are the plugin's own, and MUDling keeps them for the
character, except where it knows the shape:

| Store | What the core does with it |
| --- | --- |
| `character` | Stats. `name` names the character. |
| `skills` | `<skill>.learned`, `<skill>.level` and `<skill>.mana` fill the skills table; `practice.sessions` is what's left to spend. |
| `affects` | `<n>.name` and `<n>.detail`, then `count`, replace the whole list. |
| `worn` | `<n>.slot` and `<n>.item`, then `count`. |
| `gear`, `identify` | What's known about items, shared by a server's characters. The [Avatar README](../../plugins/avatar/README.md#stores) lists their keys. |

Writing `<reader>.read`, in `character` or `skills`, says a table finished.

### The sandbox

Each plugin has its own Luau VM, with read-only globals and only the `string`,
`table`, `math`, `utf8` and `bit32` libraries. There's no `io`, `os`, `debug`,
`require`, `loadstring` or `print`, so no file or network access.

### Limits

| Limit | Value |
| --- | --- |
| Interrupt ticks per hook call | 4000 |
| CPU time per hook call | 2 ms |
| Memory per plugin | 8 MB |
| Events waiting for the worker | 1024 |
| Commands per second, per plugin | 5 |
| Overruns in a session before the plugin is turned off | 3 |
| Added per line at p99, per plugin | 5 µs |

A tick is about one loop iteration or function call, or one step of a `string`
pattern match. A hook past its limits is stopped. When the queue is full, a new
event is dropped, so a hook can miss one: see `Readers.step` in the
[cookbook](cookbook.md#a-table-from-toml-to-hook).

## Layers

Run order is `default`, then `game`, then `override`, and within a layer the
order plugins were added. Where many values apply (patterns, coloring,
readers), all of them run in that order. Where only one can apply (the
separator, room identity, the pager, the area name, the encoding), the highest
layer wins.

## Commands

| Command | What it does |
| --- | --- |
| `mudling plugin new <id> [--dir <parent>]` | Creates `<parent>/<id>/`, which must not exist. |
| `mudling plugin test [<folder>] [--log <file> \| --update]` | Replays logs through the plugin in `<folder>`, by default the current one. |

`plugin test` has three modes:

- With no flags, it replays each `tests/*.txt` and compares what happened with
  its `tests/*.expected`. A plugin with no tests fails.
- With `--update`, it rewrites those `.expected` files to what happened instead.
  Read the difference before you commit it.
- With `--log <file>`, it replays one log and compares nothing.

A log is a session log or plain server text, one line per line. Notes such as
`[connected]` and `[GMCP] …` are skipped. Commit only small made-up text in
`tests/`. Real session logs hold other players' chat and never go in a repo,
which is why tests are `.txt`: this repo ignores `*.log`.

### What a replay does and doesn't run

It runs the line pipeline, the plugin host beside the built-in defaults, and
your hooks, with the pager answered and the plugin approved. It has no
connection, so `mud.state` returns `nil` and GMCP is skipped. `area_name` and
`room_title` read GMCP, so they show as `skipped`. The commands' rate limit is
off, because a log doesn't arrive at the pace it was sent.

### Output

Each line starts with a fixed word, or is indented under a `caught` line, and a
`fail` line holds its whole reason, so a script can read them:

| Line | Meaning |
| --- | --- |
| `plugin <id> <version>` | It loaded. |
| `log <file>: <n> lines` | A replay starts. |
| `caught <n> <place>#<i> [<event>]` | The `i`th pattern of that list matched `n` lines. Up to three follow, indented, each with its captures. `0` is no failure: a log needn't reach every pattern. |
| `skipped <place>#<i>: <why>` | A log can't show this pattern. |
| `cost +<µs> µs a line at p99 (…), budget <µs> µs` | Within budget. |
| `updated <file>` | `--update` rewrote it. |
| `fail load: <why>` | It didn't load, or a pattern didn't compile. The reason names the file. |
| `fail budget: <why>` | It's over the line budget, or a hook overran. |
| `fail hook: <why>` | A hook raised an error. |
| `fail expected: <why>` | A transcript isn't what `.expected` says. |
| `fail tests: <why>` | It has no tests. |
| `passed` or `failed` | The last line. |

An `.expected` file holds the transcript, a line for each event and hook output,
in order: `event <point> <name> <capture>="…"`, `store <store> <key> <value>`,
`send "<command>"`, `refresh_inventory`, `hook_failed <hook>` and
`plugin_disabled <id>`.

| Exit code | Meaning |
| --- | --- |
| 0 | Passed. |
| 1 | The plugin failed: see the `fail` lines. |
| 2 | The command couldn't run: bad arguments, an unreadable or empty log, or a folder that already exists. It says why on stderr. |

### The line budget

The cost is what the plugin adds to the host's p99 per line over the log, read
in turns with and without it: the bench's measure, over the best of up to five
rounds. Hooks run off the line thread, so they don't count here. Their own
limits do.

Trust a release build. A debug build reads lines about ten times slower, so it's
held to 100 µs instead of 5 µs and says so in its `budget` figure. Timing also
depends on the machine: measure on a quiet one, with a log like the game's
text.
