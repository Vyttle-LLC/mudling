# plugins/aardwolf

The Aardwolf game plugin (aardmud.org): TOML only, written against the public
plugin API (PRD §11). Aardwolf is a heavily customized ROM derivative with
rich GMCP, the other end of the range from [Avatar](../avatar), which MUDling
reads mostly from its text. Here GMCP carries most of it, and the core reads
GMCP itself, so the plugin is small: the prompt, the exits line, the moves
the game refuses, and where each vital is.

It has no hooks, so it needs no permissions. It isn't bundled with the
desktop app; add it from this folder (see the
[reference](../../docs/plugins/reference.md#quick-start)).

## Files

| File | What it declares |
| --- | --- |
| `plugin.toml` | The manifest: Aardwolf's host and MSSP name, its points, and the `prompt` fragment. |
| `text.toml` | The prompt, known by its shape. |
| `map.toml` | The exits line and the moves the game refuses. Rooms are known by room.info's number, the default. |
| `hud.toml` | Where each vital's maximum is, and experience. |

## What comes from where

| What | From | Read by |
| --- | --- | --- |
| The room's name, zone and terrain, the player's place on the map | GMCP `room.info` | The core |
| Each exit and the room it leads to | `room.info`'s `exits`, room numbers | The core |
| The exits line, closed doors in parentheses | Text: `[ Exits: north east (south) ]` | `map.toml`: `map.exits` |
| Moves the game refuses | Text: `Alas, you cannot go that way.` and others | `map.toml`: `map.failed_move` |
| HP, mana and moves | `char.vitals`: `hp`, `mana`, `moves` | The core, by their common names |
| Their maximums | `char.maxstats`: `maxhp`, `maxmana`, `maxmoves` | `hud.toml` |
| Experience to the next level, and a level's whole | `char.status`'s `tnl`; `char.base`'s `perlevel` | `hud.toml` |
| The level | `char.status`'s `level` | The core |
| The prompt | Text: `<400/420hp 610/610mn 515/530mv 980tnl>` | `text.toml` |

The room's title comes from `room.info`'s `name`. In text it's a plain line
with nothing to tell it from any other, so the plugin doesn't read it there.

## Events

| Point | Event | Captures | Line |
| --- | --- | --- | --- |
| Map | `map.exits` | `exits` | `[ Exits: north east (south) ]` gives `north east (south)` |
| Map | `map.failed_move` | | `Alas, you cannot go that way.`, `There is no exit in that direction.`, `You need to use a boat, fly, or swim underwater to go there.`, `You must be standing first.`, `You are regaining balance and are unable to move.` |

## Sources

No agent connected to Aardwolf. Each format comes from a public page, read for
its shape only; the fixtures are made-up text in that shape.

| Format | Public source | Where it's used |
| --- | --- | --- |
| `char.base`, `char.vitals`, `char.maxstats`, `char.stats`, `char.status`, `room.info`, `comm.channel`, and the module names in lowercase | Aardwolf wiki, [Clients/GMCP](https://www.aardwolf.com/wiki/index.php/Clients/GMCP) | `tests/sample.txt`'s `[GMCP]` notes; `hud.toml` |
| Subscribing with `Core.Supports.Set [ "Char 1", "Comm 1", "Room 1" ]` | [`aard_GMCP_handler.xml`](https://github.com/fiendish/aardwolfclientpackage/blob/master/MUSHclient/worlds/plugins/aard_GMCP_handler.xml) in fiendish/aardwolfclientpackage | The core's own `Char 1` and `Room 1` cover the plugin |
| The prompt's tokens and the example `10hp 10mn 10mv qt0>` | Aardwolf wiki, [Help/Prompt](https://www.aardwolf.com/wiki/index.php/Help/Prompt) | `plugin.toml`'s `prompt`; `tests/sample.txt`, `tests/text.rs` |
| `[ Exits: north east other ]`, and closed doors in parentheses | Aardwolf wiki, [Help/Exits](https://www.aardwolf.com/wiki/index.php/Help/Exits) | `map.toml`; `tests/sample.txt`, `tests/text.rs` |
| The refused moves | The speedwalk triggers in [`aard_GMCP_mapper.xml`](https://github.com/fiendish/aardwolfclientpackage/blob/master/MUSHclient/worlds/plugins/aard_GMCP_mapper.xml) | `map.toml`; `tests/sample.txt`, `tests/text.rs` |
| The host, `aardmud.org`, and that the game ends its prompt with GA (`convert_ga_to_newline`) | [`Aardwolf.mcl`](https://github.com/fiendish/aardwolfclientpackage/blob/master/MUSHclient/worlds/Aardwolf.mcl) in the same package | `plugin.toml` |

## Not confirmed

Michael's playtest confirms these:

- **The MSSP name.** No public page gives Aardwolf's MSSP `NAME`;
  `"Aardwolf"` is a guess. The host matches either way.
- **The prompt's default.** `help prompt` says `prompt all` sets "hp mana
  moves tnl" but doesn't show it. The `prompt` fragment reads any prompt that
  starts with hit points, mana and moves in `help prompt`'s tokens and ends at
  `>`; a player's own prompt may not.
- **A closed door's parentheses.** Help/Exits says closed doors are in
  parentheses but shows none, so `(south)` is a guess at the form.
- **The room's layout in text.** The fixtures print the name, the
  description, then the exits line, as ROM does. No public page shows a
  whole room.
- **A maze room's exits.** The GMCP page says maze rooms leave out the room
  numbers, but not what they send instead.

## What the API can't say yet

API v1 hands a plugin only text: its patterns read lines, and its hooks see
only what they match. GMCP goes to the core, which reads fixed names in it,
the ones Avatar sends. Where Aardwolf's names differ, nothing in a plugin can
point the core at them, so these don't reach MUDling:

| What | Aardwolf sends | What the core reads | Effect |
| --- | --- | --- | --- |
| Chat channels | `comm.channel`: `chan`, `msg`, `player` | Nothing: the API has no point for channels | No channel view. The plugin doesn't subscribe to `Comm 1`, since nothing would read it. |
| Stats | `char.stats`: `str`, `int`, `wis`, `dex`, `con`, `luck`, `hr`, `dr`, `saves` | The same names, but only from `Char.Status` | The character sheet shows no stats. |
| The character's name, class and race | `char.base`: `name`, `class`, `race` | `Char.Status`'s `character_name`, `class`, `race` | The character isn't named from GMCP, and isn't saved by name. |
| Position | `char.status`'s `pos` | `Char.Vitals`'s `position` | No position. |
| Alignment, gold, practices | `char.status`'s `align`; `char.worth`'s `gold`, `pracs` | `Char.Status`'s `alignment`, `gold`, `practice` | Not on the sheet. |
| The opponent | `char.status`'s `enemy`, `enemypct` | `Char.Status`'s `opponent_name`, `opponent_health`, `opponent_health_max` | No opponent bar. `hud.toml`'s `opponent` is declared but nothing reads it. |
| Area names | `room.area`, only when asked with `request area` | A plugin can't send GMCP or read it | Areas show as `room.info`'s zone id, such as `hedgerow`. `map.toml`'s `areas` could name them, but no public page lists the ids. |

The Map point's `exits` rule also assumes the room's description follows the
exits line, as in Avatar. Aardwolf prints it before, so the map reads no
description. Room numbers tell rooms apart without one, so nothing is lost
here, but a game that prints exits last and has no room numbers couldn't be
mapped by description.

The smallest general fix for most of the table is a GMCP field map for the
character sheet, like `hud.toml`'s `vitals`: each sheet key and the field that
holds it, `str = "char.stats.str"`, `name = "char.base.name"`, read by the
code that reads `Char.Status` today. Reading `hud.toml`'s `opponent` the same
way would give the opponent bar. Chat channels need a point of their own.

## Tests

`cargo test -p mudling-plugin-aardwolf` loads this folder with `Plugin::load`.
`tests/text.rs` runs text through the line pipeline and the plugin host.
`tests/gmcp.rs` feeds `tests/sample.txt`, its text and its GMCP, through a
session engine with the plugin turned on, and reads the map, vitals and
character stores; nothing opens a socket. `tests/plugin.rs` runs the checks a
plugin added from a link must pass.

`mudling plugin test plugins/aardwolf` replays `tests/sample.txt`'s text and
compares what it produces with `tests/sample.expected`. A replay skips GMCP:
see the [reference](../../docs/plugins/reference.md#what-a-replay-does-and-doesnt-run).

## Cost

Measured as Avatar's was, in a release build on an Apple Silicon Mac shared
with other builds (load average 16 to 23), on 2026-11-02:

| Measure | Text | Added per line at p99 |
| --- | --- | --- |
| `mudling plugin test` | `tests/sample.txt`, 11 lines | +0.71 µs |
| `mudling-bench --plugin` | the bench's fixture, 5,059 lines | +0.00 µs (Avatar: +0.17 µs) |

Both are within the 5 µs budget. The bench's fixture is Avatar-like text,
which these patterns mostly don't match, so the sample is the closer
measure of Aardwolf's own text.
