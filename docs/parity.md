# Parity with the prototype

PRD §7's parity test: the prototype's session logs replay through MUDling
with the Avatar plugin, and the final map, character records and gear
database match what the prototype produced, or differ only in documented,
intended ways. This file is that documentation.

```sh
cargo run --release -p mudling-plugin-avatar --bin avatar-parity
```

It writes `report.md` and `differences.json` to the user cache folder
(`~/Library/Caches/mudling-parity` on macOS, or `--out <folder>`), with every
difference named. Both hold game text, so they stay out of the repo. It exits
non-zero on a kind of difference this file doesn't explain, or on a notice
from a session. The replay runs far faster than a server, so it pauses around
each identify, whose hook reads what's at hand, and runs repeat.

## The baseline

"What the prototype produced" is **the prototype's own final code reading
the same logs**, not its saved world.

- The checker copies `mud-client-spike/Sources/MUDKit` to the cache folder
  and builds `plugins/avatar/parity/baseline.swift` beside it. The spike
  isn't built or changed. The first run takes about 6 minutes; the result is
  kept until the sources or the logs change.
- `baseline.swift` calls MUDKit in the order `AppModel.handle(_:)` does: each
  log is one connection, on a world saved and loaded between them.
- The saved world is a second reference only. It's copied before it's read,
  and the report counts how it differs. It can't be the baseline: it was
  repaired by hand (172 rooms still have no description, and rooms were
  forgotten and maze marks cleared), and each log was played with whatever
  the code was that day.

What the logs can't show, on either side: anything typed but a move or a
look. So a typed identify can't name its copy, and the baseline sends no
commands.

Both sides read each log the same way. The logger wraps its notes in newlines
of its own, so what lies between two notes is the server's text exactly, and
a prompt left unended stays unended. MUDling's side feeds that to `Engine`
as telnet bytes, with a GMCP frame for each `[GMCP]` note and `Engine::send`
for each `[MAP]` note, one `Engine` per log on one data folder.

## The logs

The PRD counted 84 logs; there are 95 now. **The extra 11 are the logs of
October 29**, the day the PRD was committed: the 84 end at
`session-20261028-225005`. One of the 11, `session-20261029-110522`, is
empty. All 95 are used.

## Result

| | Rooms | Links | Maze squares | Characters |
| --- | --- | --- | --- | --- |
| Prototype's code | 1,637 | 3,194 | 8 | 4 |
| MUDling | 1,637 | 3,194 | 8 | 4 |
| Saved world | 1,459 | 3,022 | 2 | 4 |

The final maps match in every field the checker compares (exits, links,
positions, buildings, doorways, maze and tangle marks, features, sightings,
environment) except two descriptions, below. Aggressive mobs match. Identifies match:
every reading MUDling keeps is one the prototype has.

Room ids don't match, and aren't compared: the prototype spends two on Lost in
the Dark Woods before it works out that's a maze, where the plugin declares
it. The checker pairs rooms by identity, in order of id.

## Differences

All 245 are intended. "Prototype" means its code on these logs.

| Kind | Count | Sample | Why |
| --- | --- | --- | --- |
| `character.skill` | 126 | `heighten senses`: learned none / 90; `hide`: learned none / 95; `identify`: none / 95, level 51, mana 12 | MUDling is right. The prototype takes the pager's line as the blank that ends `practice`, so it never reads page two, though the rows are in the log. All 126 are the Kzinti hero's, all none / a value. |
| `character.stat` | 47 | `class`: none / `Monk`; `level`: none / 51; `race`: none / `Lesser Imp`; `bank`: none / 6853 | MUDling is right. It reads three characters' scores the prototype missed: see [score](#the-score-pattern). |
| `character.affect` | 15 | `adrenaline pump`, `anticipate`, `body brace`: none / on | MUDling is right. It's `aff` after a prompt that leaves a space in front of the next line, which the prototype misses. |
| `character.worn` | 13 | `worn.about.1`: none / a linen robe; `worn.head.1`: a Tiamat mask / an official Cult of Bonk Hat; `worn.wield.2`: an alligator tooth / none | MUDling is right. It reads `eq` after that same prompt, which the prototype never did. So the prototype's list for this character is only what wear messages added across every log, never corrected by a table: it shows four weapons at once. MUDling's is the last table. |
| `character.title` | 2 | `the donation room for players level 1 through 10.  Items in` / `the Violent Missionary`; none / `The Lost` | MUDling is right. The first is room text the prototype takes for `score`: "You are in the donation room…". The second is a title with a capital. |
| `gear.measured` | 38 | a braying mule tattoo, a bright ball of light, a copper bracelet | Not built yet. The prototype learns what an item does from Char.Status before and after wearing it. Hooks don't see Char.Status; this belongs to Phase 5's gear work. |
| `gear.copies` | 1 | 78 copies by game id / none | Not built yet. Phase 5 fills typed `GearCopy` records; hooks write generic store values today. |
| `gear.slot` | 1 | a Golden Unicorn Horn: `wield` / `offhand` | MUDling is right: its `eq` table says `<held in offhand>`. The prototype never read that table and took the slot from a wear message. |
| `map.description` | 2 | `A waiting room ` (The Orphanage), `Through the woods ` (Paradise Point): empty / the room's text | MUDling is right. Avatar's Room.Info gives these names a trailing space, and the prototype compares them with trimmed lines. |

The differences these replaced were fixed, not excused:

| Was | Fix |
| --- | --- |
| 1,047 rooms without sightings, 129 without features | `LiveMap::gmcp` reads Room.Players, Room.AddPlayer and Char.Items. |
| 19 rooms only in the prototype, 64 links to the wrong room | A door changing in place was missed, so the map lost its way. It now reads Char.Status `room_exits`. |
| 29 rooms moved, 4 rooms in a different building | `LiveMap::load` now rebuilds buildings, as the prototype does on every load. |
| One "It is pitch black..." room | Dark-room text after a custom prompt starts with a space. |
| 2 weapons with no slot | `examine`'s "wielded as a weapon" names a slot now. |
| 17 items with no keywords | The plugin ties a spell identify to the item at hand, by the prototype's rule. |

## Questions settled

### Look-alike rooms

1,648 rooms against 1,459 saved isn't a port bug: the prototype's code makes
the same copies. After the fixes above, both give 1,637.

207 identities (name, zone, description and exits) are shared by 523 rooms. A
pair is told apart when walking the same way from each reaches rooms that
differ.

- **146 identities are true look-alikes**: every pair is two places. 9 × "A
  narrow trail", 7 × "Farmer's Way" and 7 × "Dwarven Trade Route" are stretches
  of road, each square the same as the next.
- **61 identities hold 143 rooms nothing tells apart**, so up to 82 rooms are
  copies. The most are in Westwood (34 rooms), The Meadow (18), Ruins of
  Westwood (16) and the Dwarven Day Care (14), plus 4 × Donation Room, below.
  Some will be two places whose differences no one walked.

The saved world's gap is hand repair on top: it keeps 34 such identities (74
rooms), and it also lacks rooms the replay has and has 58 of its own.

**For VYT-227:** of 1,321 identities, 1,114 belong to one room and can unite.
The 207 others won't, as a merge needs one room on each side: 146 of them
rightly, and 61 are copies a merge would keep.

### Maze squares

There are 8 now, in both clients: Lost in the Dark Woods, which the plugin
declares, and West Side of Tree of Knowledge, Stonehall, Northern Meadow,
Aelmon's Sanctuary, Marketplace., A crumbling hallway and Southern Qanza
Prairie. The other 7 come from `collapse_maze`'s structural check, which the
prototype has too. They're false marks where the walk tangled, not a port bug.
The saved world holds 2 because they were cleared by hand. MUDling keeps the
prototype's behaviour here.

### Rooms with no description

4 remain, all Donation Room. A character can log in there, and the first
Room.Info of a connection has no room text before it. An empty description
can't match the described room, so each makes a room of its own. The prototype
does the same. The 2 with a trailing space are fixed, above. The pitch-black
room is gone: dark rooms aren't mapped.

### A table left at its pager

`avatar-replay` counts these again: it notes a pager followed by no row. There
are 30: 25 `practice`, 5 `slist`. In these logs the prototype left the pager
unanswered, or the next page never came. The replay answers every pager, as a
live client does, so a table like this ends at the next prompt with the rows it
got. Rows are kept by key, so nothing already known is lost. MUDling answers
the pager live, so this only happens in a log.

### The score pattern

The stricter pattern did miss real scores, and so did the prototype's. Every
one of the 429 scores in the logs is now read:

| Line | Missed by |
| --- | --- |
| `You are Fen the unknown` | The plugin (capital required) |
| `Level 51(14) Kzinti Monk` | The plugin's row, and both clients' level |
| `You are Fen The Lost` | Both |
| ` You are Fen the Violent Missionary`, after `<46/46hp><116/116m><116/116mv>` | Both |
| `$*(-10h-133m-104v-402)(-/-)You are …` | The plugin's prompt shape |

A race of two words, "Level 4 Lesser Imp Monk", is read whole too; the
prototype had race `Lesser` and class `Imp`. Room text stays out: "You are in
the donation room…" has no capital after "are".
