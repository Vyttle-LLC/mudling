# plugins/avatar

The Avatar game plugin (avatar.outland.org): TOML with Luau hooks, written
only against the public plugin API (PRD §11). Everything MUDling knows about
Avatar is in this folder; the core holds none of its wording.

It's also the first worked example for plugin authors (the cookbook,
VYT-235): each file says what it reads, and each regex has a comment.

## Files

| File | What it declares |
| --- | --- |
| `plugin.toml` | The manifest: Avatar's hosts and MSSP name, its points, the stores its hooks write. |
| `text.toml` | Everything read line by line: the prompt and pager, each table's start and rows, and the game's messages. |
| `readers.luau` | Framing for tables: a start opens a reader, rows add writes, the next prompt sends them. |
| `character.luau` | `score`, `practice`, `slist` and `aff`. |
| `gear.luau` | `equipment`, where items go, identify, examine, and the word a command uses for an item. |
| `commands.toml` | Verbs and door commands. No separator and no speedwalk: one command a line. |
| `character.toml` | The background commands and the table that answers each. |
| `gear.toml` | The 15 slots in the game's order. |
| `map.toml` | Rooms known by description; the maze. |
| `mobs.toml` | A mob's name from its room title. |

Only the Text point runs per line today, so every line reader is a Text
pattern; `character.toml`'s readers repeat the same patterns, and a test
keeps them identical.

## How a table is read

Avatar sends no GA or EOR, so its prompt is known by its shape
(`[[prompt]]` in `text.toml`). The client ends a line there; in a session
log the next output is glued after it, so the patterns for a table's first
line allow a prompt in front.

A table's start pattern opens a reader, its row patterns add writes, and
the next prompt (or the next table's start, for a prompt the plugin doesn't
know) sends them all at once, ending with the table's `read` or `count`
key. A table left at an unanswered pager (`[C]ontinue:` followed by a
prompt rather than more rows) was cut short and sends nothing. The client
answers the pager with `c` while a table is open.

## Events

Each Text pattern's match is a `PluginEvent` named below, its named
captures as data. Hooks run on the ones marked †; the rest are for stores
to consume.

| Event | Captures | Line |
| --- | --- | --- |
| `prompt` † | | A prompt, bare or with output after it. Ends the open table. |
| `pager` † | | The pager's prompt, once answered. |
| `character.score.start` † | `name`, `title` | `You are Tamsin the Quiet Attendant` |
| `character.score.row` † | | `Level 12 Harpy Monk …`, `Str : 15 (18) …`, `Gold : …` |
| `character.practice.start` † | `sessions` | `You have 12 practice sessions left.`, the header |
| `character.practice.row` † | | `dodge  95  4  -  0  kick  90  7  -  0` |
| `character.slist.start` † | | `Spells and skills learnable by your class …` |
| `character.slist.row` † | | `  3:       fast healing        meditation`, wrapped rows |
| `character.slist.blank` † | | A blank line, which ends a run of wrapped rows |
| `character.affects.start` † | | `You are affected by:` |
| `character.affects.none` † | | `You are not under the affects of any spells` |
| `character.affects.row` † | `name`, `detail` | `Spell: 'bless'.` |
| `character.skill_practiced` † | `skill`, `learned` | `You practice dodge with Ulric. Your current skill level is now 45.` |
| `character.skill_improved` | | `You have become better at …` (ROM wording) |
| `character.affects_changed` | | `You feel …`, `… wears off` |
| `gear.equipment.start` † | | `You are using:` |
| `gear.equipment.row` † | `slot`, `item` | `<worn on finger>    a pink ice ring` |
| `gear.wore` † | `item`; `on`, `part` for wear | `You wear …`, `You hold …`, `You light … and hold it.`, `You wield …` |
| `gear.removed` | `item` | `You stop using a round shield.` |
| `gear.put` | `item`, `container` | `You put a rope belt in a satchel.` |
| `gear.did_not_fit` | | `It won't fit.` |
| `gear.not_carried` | | `You do not have that item.` |
| `gear.identify.named` † | `item` | `Ulric looks closely at your bear skin bracelet.` |
| `gear.identify.object` † | `keywords`, `type`, `flags` | `Object 'black bear bracelet' type armor, extra flags none.` |
| `gear.identify.level` † | `level` | `Weight 3, value 40, level 8.` |
| `gear.identify.quality` † | `quality` | `Object Quality (100 / 100 hps)` |
| `gear.identify.armor` † | `armor` | `Armor class is 2.` |
| `gear.identify.base` † | `stat`, `amount` | ` Modifies strength by  1 continuous` |
| `gear.identify.added` † | `spell`, `stat`, `amount` | `'enchant armor  '  modifies armor class by  -4 continuous` |
| `gear.examine.item` † | `item` | `You look at a linen robe in your inventory...` |
| `gear.examine.slot` † | `meant` | `This item appears meant to be worn about the body.` |
| `map.area` | `low`, `high`, `name` | `{10  15} Dev     Sarphyre's Court` |
| `map.failed_move` | | `No way!  You are still fighting!`, `You are too exhausted.` |
| `map.darkness` | | `It is pitch black...` |
| `map.flee` | `dir` | `You flee north! What a COWARD! …` |
| `mobs.aggro` | `name`, `noun` | `The nanny GLARES at you and SNARLS!` |

`mobs.toml`'s `mobs.name` rule is applied to a mob's room title, not to
lines: `(White Aura) A myconoid shaman is here, chanting.` gives `name` =
`A myconoid shaman`. Compare names ignoring case.

## Stores

Hooks write booleans, numbers and strings as `StoreWrite`s. A table's
writes arrive together, its `read` or `count` key last: a store applies the
batch when that key arrives. An item's *plain name* is its name without
tags such as `(Glowing)`, spaces collapsed, in lowercase.

**`character`**, from `score`:

| Key | Value |
| --- | --- |
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

**`equipment`**, from `equipment`; every row, in the game's order:

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
- **Area names over GMCP**: Avatar's Room.Info `zone` uses the format
  `map.area` reads, but plugins can't declare patterns over GMCP values yet.
- **Which copy a spell's identify was about**: that needs the inventory,
  which no state view offers yet.

## Tests

`cargo test -p mudling-plugin-avatar` loads this folder with `Plugin::load`
and runs scrubbed fixtures (`tests/fixtures/`) through the line pipeline,
the plugin host and the Luau hooks. Fixtures are the game's own tables and
messages with player-specific values changed; never add session logs, other
players' names, or channel text.

`cargo run --release -p mudling-plugin-avatar --bin avatar-replay -- <logs…>`
replays local session logs and counts the reads each table completed. It
reads logs at runtime only.
