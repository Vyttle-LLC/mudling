# plugins/aardwolf

The Aardwolf game plugin (aardmud.org): TOML only, written against the public
plugin API (PRD §11). Aardwolf is a heavily customized ROM derivative with
rich GMCP, the other end of the range from [Avatar](../avatar), which MUDling
reads mostly from its text. Here GMCP carries most of it, and the core reads
GMCP where the plugin says, so the plugin is small: the prompt, the exits
line, the moves the game refuses, the channels, the areas' names, and where
each vital, each of the character's values and the opponent are.

It has no hooks. Its one permission, `send_gmcp`, sends `request area` over
GMCP, so it asks for the player's approval when it's installed or turned on.
It isn't bundled with the desktop app; add it from this folder (see the
[reference](../../docs/plugins/reference.md#quick-start)).

## Files

| File | What it declares |
| --- | --- |
| `plugin.toml` | The manifest: Aardwolf's host and MSSP name, its permission, its points, and the `prompt` fragment. |
| `connection.toml` | The `Comm 1` module, and `request area` for each zone entered. |
| `text.toml` | The prompt, known by its shape, and the channels from comm.channel. |
| `map.toml` | The areas' names from room.area, the exits line and the moves the game refuses. Rooms are known by room.info's number, the default. |
| `hud.toml` | Where each vital's maximum is, experience, the character sheet's values and the opponent. |

## What comes from where

| What | From | Read by |
| --- | --- | --- |
| The room's name, zone and terrain, the player's place on the map | GMCP `room.info` | The core |
| Each exit and the room it leads to | `room.info`'s `exits`, room numbers | The core |
| The area's name | `room.area`: `id`, `name`, sent after `request area` | `connection.toml` asks; `map.toml`: `area_info` |
| Channels, tells, says and yells | `comm.channel`'s `msg`, with `Comm 1` on | `text.toml`: `channel` |
| The exits line, closed doors in parentheses | Text: `[ Exits: north east (south) ]` | `map.toml`: `map.exits` |
| Moves the game refuses | Text: `Alas, you cannot go that way.` and others | `map.toml`: `map.failed_move` |
| HP, mana and moves | `char.vitals`: `hp`, `mana`, `moves` | The core, by their common names |
| Their maximums | `char.maxstats`: `maxhp`, `maxmana`, `maxmoves` | `hud.toml` |
| Experience to the next level, and a level's whole | `char.status`'s `tnl`; `char.base`'s `perlevel` | `hud.toml` |
| The level | `char.status`'s `level` | The defaults' `Char.Status.level` |
| The character's name, race and class | `char.base`: `name`, `race`, `class` | `hud.toml` |
| Stats | `char.stats`: `str`, `int`, `wis`, `dex`, `con`, `luck`, and `hr`, `dr`, `saves` as `hitroll`, `damroll`, `saves` | `hud.toml` |
| Position and alignment | `char.status`'s `pos`, `align` | `hud.toml` |
| Gold and practices | `char.worth`'s `gold`, `pracs` | `hud.toml` |
| The opponent | `char.status`'s `enemy`, and its health as a percentage, `enemypct` | `hud.toml` |
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
| Subscribing with `Core.Supports.Set [ "Char 1", "Comm 1", "Room 1" ]` | [`aard_GMCP_handler.xml`](https://github.com/fiendish/aardwolfclientpackage/blob/master/MUSHclient/worlds/plugins/aard_GMCP_handler.xml) in fiendish/aardwolfclientpackage | `connection.toml` adds `Comm 1` to the core's own |
| `request area`, a GMCP request among the wiki's Aardwolf GMCP Commands, sent on entering a new zone; `room.area`'s `id` and `name` | The Aardwolf wiki's [Clients/GMCP](https://www.aardwolf.com/wiki/index.php/Clients/GMCP); `Send_GMCP_Packet("request area")` and `update_gmcp_area` in [`aard_GMCP_mapper.xml`](https://github.com/fiendish/aardwolfclientpackage/blob/MUSHclient/MUSHclient/worlds/plugins/aard_GMCP_mapper.xml) | `connection.toml`, `map.toml`; `tests/sample.txt` |
| comm.channel's `msg` shown in the main window, from GMCP | `echo_msg` in [`aard_chat_echo.xml`](https://github.com/fiendish/aardwolfclientpackage/blob/MUSHclient/MUSHclient/worlds/plugins/aard_chat_echo.xml) | `text.toml`; `tests/sample.txt` |
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
- **Channels only as GMCP.** The game's own client package shows each
  comm.channel's `msg` in the main window itself, which suggests that with
  `Comm 1` on the game leaves channels out of its text. If it sends both,
  each channel shows twice; a text rule can't hide the text copy yet.
- **A channel's colors.** The GMCP page says GMCP colors are ANSI or raw, by
  an option. The plugin reads ANSI, which the fixture assumes is the default;
  raw codes would show as written.

## Follow-ups

Writing this plugin found where API v1 was shaped around Avatar. The core
read the character sheet, the opponent, gear's stats and doors from Avatar's
GMCP names; it now reads them where a plugin says, and API v1 gained
channels that come as GMCP and GMCP requests, which this plugin uses for
comm.channel and `request area`. One thing remains, which no v1 plugin will
need to change for:

| What | Aardwolf sends | What's missing | Effect now |
| --- | --- | --- | --- |
| The room's description | The description, then the exits line | An `exits` rule for exits printed after the description | The map reads no description. Room numbers tell rooms apart without one, so nothing is lost here, but a game that prints exits last and has no room numbers couldn't be mapped by description. |

## Tests

`cargo test -p mudling-plugin-aardwolf` loads this folder with `Plugin::load`.
`tests/text.rs` runs text through the line pipeline and the plugin host.
`tests/gmcp.rs` feeds `tests/sample.txt`, its text and its GMCP, through a
session engine with the plugin turned on and approved, and reads the map,
vitals, character, opponent and chat stores, and what the client sent;
nothing opens a socket. `tests/plugin.rs` runs the checks a plugin added
from a link must pass.

`mudling plugin test plugins/aardwolf` replays `tests/sample.txt`'s text and
compares what it produces with `tests/sample.expected`. A replay skips GMCP:
see the [reference](../../docs/plugins/reference.md#what-a-replay-does-and-doesnt-run).

## Cost

Measured as Avatar's was, in a release build on an Apple Silicon Mac shared
with other builds (load average 14 to 19), on 2026-11-02:

| Measure | Text | Added per line at p99 |
| --- | --- | --- |
| `mudling plugin test` | `tests/sample.txt`, 12 lines | +0.75 µs |
| `mudling-bench --plugin` | the bench's fixture, 5,059 lines | +0.00 µs (Avatar: +0.13 µs) |

Its GMCP fields cost no line anything: the core reads them only when a GMCP
message arrives.

Both are within the 5 µs budget. The bench's fixture is Avatar-like text,
which these patterns mostly don't match, so the sample is the closer
measure of Aardwolf's own text.
