# plugins/avatar

The Avatar game plugin (avatar.outland.org): TOML with Luau hooks, written
only against the public plugin API (PRD §11). Everything MUDling knows about
Avatar is in this folder; the core holds none of its wording.

It's also the first worked example for plugin authors (the cookbook,
VYT-235): each file says what it reads, and each regex has a comment.

The desktop app ships it compiled in (`bundled()` in `src/lib.rs`, which names
every file here, so a new file goes there too). It stays off until the player
turns it on from the suggestion. A folder with its id in `<data>/plugins/`
replaces it.

## Files

| File | What it declares |
| --- | --- |
| `plugin.toml` | The manifest: Avatar's hosts and MSSP name, its points, the stores its hooks write, and the regex fragments its patterns share. |
| `connection.toml` | The login prompts, each answered once. |
| `text.toml` | The prompt and pager, and the lines no other point reads. |
| `readers.luau` | What the table hooks share: rows add writes, and the table's end sends them. |
| `character.luau` | `score`, `practice`, `slist` and `aff`. |
| `gear.luau` | `equipment`, where items go, identify and examine. |
| `commands.toml` | Verbs, door commands and the rules for an item's keyword. No separator and no speedwalk: one command a line. |
| `character.toml` | The background commands, the character's tables, and the messages about skills. |
| `gear.toml` | The 15 slots in the game's order, the gear tables, and the wear, remove and put messages. |
| `map.toml` | Rooms known by description, the zone's format, the exits line, failed moves, darkness, flees and the maze. |
| `mobs.toml` | Aggression, and a mob's name from its room title. |
| `hud.toml` | Where the game lists affects. |

## How a table is read

Avatar sends no GA or EOR, so its prompt is known by its shape, the
`prompt` fragment. The client ends a line there; in a session log the next
output is glued after it, so the patterns for a table's first line allow a
prompt in front. They allow spaces too: a custom prompt can leave one before
the next line.

The host frames each table. A start pattern opens it, and row patterns run
only while it's open. The hook collects rows as writes and sends them at
the table's end, the next prompt or the next table, ending with its `read`
or `count` key. The client answers the pager with `c` while a table is open.
A table that ends with its pager unanswered, or with the connection, is cut
short and sends nothing; a last page with no rows was still answered.

## Quiet reads

The core sends `character.toml`'s `[[command]]`s in the background and keeps
their answers out of the chat. `crates/mudling-core/src/session/quiet.rs` has
the rules. Two names matter to it, and neither needs anything but a rule's
name: a rule named `<reader>.stale` asks for that reader's command again, and
one named `<reader>.none` is a one-line answer that isn't a table. A rule of
the Gear point's `wear` and `remove` lists asks for the Gear point's command.

## Events

Each match is a `PluginEvent` named below, its named captures as data. Hooks
run on the ones marked †; the rest are for stores to consume.

Each table sends its hook `<reader>.start`, `.row`, `.gap` (the first line
that's no row), and `.end` or `.cut`:

| Reader | Hook | Start captures | Row captures | Lines |
| --- | --- | --- | --- | --- |
| `score` | `read_score` | `name`, `title` | | `You are Tamsin the Quiet Attendant`; `Level 12 Harpy Monk …`, `Str : 15 (18) …`, `Gold : …` |
| `practice` | `read_practice` | `sessions` | | `You have 12 practice sessions left.`, the header; `dodge  95  4  -  0  kick  90  7  -  0` |
| `slist` | `read_slist` | | | `Spells and skills learnable by your class …`; `  3:       fast healing        meditation`, wrapped rows |
| `affects` | `read_affects` | | `name`, `detail` | `You are affected by:`; `Spell: 'bless'.` |
| `equipment` | `read_equipment` | | `slot`, `item` | `You are using:`; `<worn on finger>    a pink ice ring` |
| `identify` | `read_identify` | `item`, or `keywords`, `type`, `flags` | `level`, `quality`, `armor`, or `stat`, `amount` and perhaps `spell` | `Ulric looks closely at your bear skin bracelet.`, `Object 'black bear bracelet' type armor, extra flags none.`; `Weight 3, value 40, level 8.`, ` Modifies strength by  1 continuous`, `'enchant armor  '  modifies armor class by  -4 continuous` |
| `examine` | `read_examine` | `item` | `meant` | `You look at a linen robe in your inventory...`; `This item appears meant to be worn about the body.` |

The rest are single lines:

| Point | Event | Captures | Line |
| --- | --- | --- | --- |
| Text | `affects.none` † | | `You are not under the affects of any spells`: `aff`'s answer when nothing's on |
| Text | `affects.stale` | | `You feel …`, `… wears off` |
| Text | `score.stale`, `practice.stale`, `slist.stale` | | `You raise a level!! …` |
| Text | `map.area` | `low`, `high`, `name` | `{10  15} Dev     Sarphyre's Court` |
| Character | `character.skill_practiced` † | `skill`, `learned` | `You practice dodge with Ulric. Your current skill level is now 45.` |
| Character | `practice.stale` | | `You have become better at …` (ROM wording) |
| Gear | `gear.wore` † | `item`; `on`, `part` for wear | `You wear …`, `You hold …`, `You light … and hold it.`, `You wield …` |
| Gear | `gear.removed` † | `item` | `You stop using a round shield.` |
| Gear | `gear.put` | `item`, `container` | `You put a rope belt in a satchel.` |
| Gear | `gear.did_not_fit` | | `It won't fit.` |
| Gear | `gear.not_carried` | | `You do not have that item.` |
| Map | `map.exits` | | `[Exits: north up]` |
| Map | `map.failed_move` | | `No way!  You are still fighting!`, `You are too exhausted.` |
| Map | `map.darkness` | | `It is pitch black...` |
| Map | `map.flee` | `dir` | `You flee north! What a COWARD! …` |
| Mobs | `mobs.aggro` | `name`, `noun` | `The nanny GLARES at you and SNARLS!` |

`mobs.toml`'s `mobs.name` rule is applied to a mob's room title, not to
lines: `(White Aura) A myconoid shaman is here, chanting.` gives `name` =
`A myconoid shaman`, which the map keeps in lowercase for its mob marks.

## Stores

Hooks write booleans, numbers and strings as `StoreWrite`s. A table's
writes arrive together, its `read` or `count` key last: a store applies the
batch when that key arrives. An item's *plain name* is its name without
tags such as `(Glowing)`, spaces collapsed, in lowercase.

The core keeps `skills` and `affects` per character in its typed tables, and
the rest as the character's stats (`crates/mudling-core/src/session/sheet.rs`).

**`character`**, from `score`:

| Key | Value |
| --- | --- |
| `name` | `Tamsin`; also from the login's "Welcome back" line |
| `title` | `the Quiet Attendant` |
| `level`, `age` | numbers |
| `race`, `class` | `Harpy`, `Monk` |
| `str`, `int`, `wis`, `dex`, `con` | current values (GMCP Char.Status keys) |
| `str_perm`, … `con_perm` | permanent values |
| `gold`, `bank`, `practice`, `wimpy` | numbers |
| `killed`, `died`, `arrests`, `incarnation` | numbers |
| `score.read` | rows read; last |

**`skills`**, from `practice`, `slist` and trainers:

| Key | Value |
| --- | --- |
| `<skill>.learned` | percent learned (`practice`, a trainer) |
| `<skill>.level` | level the class learns it (`practice`, `slist`) |
| `<skill>.mana` | mana per use; above 0 is a spell, used with `cast` |
| `practice.sessions` | practice sessions left |
| `practice.read` | skills read; last of a `practice` |
| `slist.read` | skills read; last of an `slist`. A full `slist` names every class skill: others this batch doesn't name aren't the character's, except those a `practice` read this session (race skills). |

**`affects`**, from `aff`; the whole list each time:

| Key | Value |
| --- | --- |
| `<n>.name` | `bless`, for n from 1; a spell listed twice is on once |
| `<n>.detail` | `modifies hit roll by 3 for 6 hours`, when given |
| `count` | entries; last. `0` when nothing is on. |

**`worn`**, the character's own, from `equipment`; every row, in the game's
order:

| Key | Value |
| --- | --- |
| `<n>.slot` | the slot's short name from `gear.toml`, for n from 1 |
| `<n>.item` | the item as the game shows it, tags included; `""` for an empty slot |
| `count` | rows; last. Finger, neck and wrist can show twice. |

**`gear`**, what's known about items, by plain name:

| Key | Value |
| --- | --- |
| `slot.<plain name>` | the slot it goes in: from `equipment`, a wear message or `examine` |
| `keyword.<plain name>` | the word a command names it by: the noun ending its first phrase, never a possessive while there's another word (the game refuses `remove captain's`) |
| `keywords.<plain name>` | the game's own keywords for it, from a shop's identify |

**`identify`**, by the item's keywords (`black bear bracelet`):

| Key | Value |
| --- | --- |
| `<keywords>.name` | the item's name, when a shop gave it |
| `<keywords>.type`, `.flags` | `armor`; `glow magic` (empty for none) |
| `<keywords>.level`, `.quality` | `8`; `100 / 100 hps` |
| `<keywords>.base.<stat>` | what every copy does, by GMCP stat key; armor as AC (armor 2 is `ac` −2) |
| `<keywords>.added.<spell>.<stat>` | what a spell added to this copy: `added.enchant armor.ac` = −4 |
| `read` | the keywords; last |

## Not covered, and why

- **`worth`**: neither the prototype nor the session logs have a `worth`
  table, so there's nothing to port.
- **Which copy a spell's identify was about**: hooks can read the inventory
  (`inventory.count`, `inventory.<n>.name`), with `read_state`, but working
  out the copy isn't written yet. Avatar sends no `Char.Items` when gear goes
  on or comes off, so the wear and remove hooks call
  `mud.refresh_inventory()`.
- **Locked doors**: `Room.Info` gives an exit as open or closed, never locked,
  so the Room panel can't offer `doors.unlock`. The only sign is a line after
  an `open`, such as "The door to the east is locked." Reading that needs a
  map signal API v1 doesn't have; a text rule can come in API v2.

## Tests

`cargo test -p mudling-plugin-avatar` loads this folder with `Plugin::load`
and runs scrubbed fixtures (`tests/fixtures/`) through the line pipeline,
the plugin host and the Luau hooks. Fixtures are the game's own tables and
messages with player-specific values changed; never add session logs, other
players' names, or channel text.

`cargo run --release -p mudling-plugin-avatar --bin avatar-replay -- <logs…>`
replays local session logs and counts the reads each table completed, and
the tables left at their pager. It reads logs at runtime only.

`cargo run --release -p mudling-plugin-avatar --bin avatar-parity` compares
what MUDling reads from every log with what the prototype read. See
[docs/parity.md](../../docs/parity.md).
