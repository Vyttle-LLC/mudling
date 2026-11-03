# Plugin cookbook: reading a game with Avatar

This works through [`plugins/avatar`](../../plugins/avatar), MUDling's first game
plugin, to show how a real game gets read. It uses only the public API, so
everything here is open to your plugin too. Each code block names the file it
comes from, and `crates/mudling-cli/tests/docs.rs` fails if a block stops
matching that file, so these are the plugin's real lines. The
[reference](reference.md) lists every key.

Start with `mudling plugin new mygame`, and keep a small log of made-up text
beside you as you go.

## Say which game it is

A plugin starts with its manifest. Avatar is a game plugin, for one host:

```toml plugins/avatar/plugin.toml
id = "avatar"
name = "Avatar"
version = "0.1.0"
api = 1
license = "MIT"
layer = "game"
```

```toml plugins/avatar/plugin.toml
[servers]
hosts = ["avatar.outland.org"]
mssp_names = ["Avatar"]
```

`[points]` then names a file for each extension point it uses, one file for
each kind of thing the game says. A small plugin can stop at `text.toml`.

Its `permissions` are `read_state`, so its hooks can read the character, and
the stores its hooks write. Avatar needs no
`send_commands`: it reads the game, and leaves talking to it to the player and
the core's background commands.

## Learn the prompt's shape first

Most reading depends on knowing where a prompt is, because a prompt ends a
table. A server that sends GA or EOR says so itself. Avatar doesn't, so the
plugin describes the prompt. It's used everywhere, so it's a fragment, and the
comment shows the lines it reads:

```toml plugins/avatar/plugin.toml
# Avatar's prompts, "(110h-125m-120v-1062)(-/-)" and "(599hp 1173m 514v
# 1046tnl)(-/-)", and a common custom one, two or more <...> groups with
# numbers: "<46/46hp><116/116m><116/116mv>". Tags such as "(AFK) " and
# effect marks such as "$*" may come in front. Hit points go below zero:
# "(-10h-133m-104v-402)(-/-)".
prompt = '''(?:\([A-Z]+\) )*[$*@]*(?:\(-?\d+hp?[ -]-?\d+m[ -]-?\d+v[^)]*\)\([^)]*\)|(?:<[^<>]*\d[^<>]*>){2,})'''
```

`text.toml` declares it as the prompt, and declares the pager with the key that
answers it:

```toml plugins/avatar/text.toml
[pager]
prompt = '''\[C\]ontinue: ?$'''
key = "c"
```

```toml plugins/avatar/text.toml
[[prompt]]
pattern = '''^{prompt}'''
```

## Read one line

Output often arrives glued after a prompt, so a pattern for a line that may
come first after one starts `^\s*{prompt}?\s*`. Here's the whole of what `aff`
says with nothing on:

```toml plugins/avatar/text.toml
[[pattern]]
name = "affects.none"
pattern = '''^\s*{prompt}?\s*You are not (?:under the affects of|affected by) any'''
hook = "on_no_affects"
```

The event's name does a job. `<reader>.none` tells a background read that this
one line is the whole answer, so it stays out of the chat. Its hook is one line,
and that's most hooks:

```lua plugins/avatar/character.luau
function on_no_affects(event)
    mud.store("affects", "count", 0)
end
```

## Let the point do the work

Many lines need no hook, only the right list. A rule in the Map point's `flee`
list tells the automap what happened, whatever the event is called, as long as
it captures `dir`:

```toml plugins/avatar/map.toml
[[flee]]
name = "map.flee"
pattern = '''You flee (?<dir>north|east|south|west|up|down)'''
```

Mobs work the same way, with the name and the noun to remember in one line:

```toml plugins/avatar/mobs.toml
[[aggro]]
name = "mobs.aggro"
pattern = '''^\s*{prompt}?\s*(?<name>(?:.*[^A-Za-z])?(?<noun>[A-Za-z]+)) GLARES at you and SNARLS!'''
```

## A table, from TOML to hook

A table takes three parts. Take `score`, which starts with
`You are Tamsin the Quiet Attendant`. First, the command the client sends in
the background, and the reader that takes the answer:

```toml plugins/avatar/character.toml
[[command]]
command = "score"
reader = "score"
```

```toml plugins/avatar/character.toml
[[reader]]
name = "score"
start = '''^\s*{prompt}?\s*You are (?<name>[A-Z]\S*) (?<title>[Tt]he .*?)\s*$'''
hook = "read_score"
```

The capital in `[A-Z]\S*` keeps out room text such as "You are in the donation
room". With no `end`, the next prompt ends the table.

Second, the rows. A background read hides only the lines its reader matches, so
`row` covers every line the table prints, even those that hold nothing you want.
If you miss one, it turns up in the player's chat.

Third, the hook. The host sends it `score.start`, a `score.row` for each row,
and `score.end`. Every Avatar table does the same thing with those, so
`readers.luau` keeps that in one place: it collects a table's writes, and sends
them together when the table ends. If the connection cuts the table short, it
sends nothing, because half a table would be wrong:

```lua plugins/avatar/readers.luau
function Readers.step(event, name, finish)
    local kind = string.match(event.name, "[^.]+$")
    local open = reading and reading.name == name
    if kind == "start" and not open then
        reading = { name = name, writes = {}, rows = 0, finish = finish }
        open = true
    elseif kind == "end" then
        Readers.finish()
        open = false
    elseif kind == "cut" then
        reading = nil
        open = false
    end
```

Hooks can miss events if the queue is full, so it checks that what's open is
its own table. A stray row is ignored. Then `read_score` is short, and writes
`score.read` last, so the core knows the table is done:

```lua plugins/avatar/character.luau
local finish_score = Readers.count_rows("character", "score.read")

function read_score(event)
    local kind, reader = Readers.step(event, "score", finish_score)
    if kind == "start" then
        Readers.write(reader, "character", "name", event.captures.name)
        Readers.write(reader, "character", "title", event.captures.title)
    elseif kind == "row" and read_score_row(reader, Readers.body(event)) then
        Readers.row(reader)
    end
end
```

Keep hooks' stores in the shapes the [reference](reference.md#stores) lists,
and the core files them without knowing the game.

## Keep a table fresh

A rule named `<reader>.stale` asks for that reader's command again, with no
hook. Avatar uses `skill_improved`, because the ROM wording gives no new
percentage, so the only way to learn it is to read `practice` again:

```toml plugins/avatar/character.toml
[[skill_improved]]
name = "practice.stale"
pattern = '''You have become better at|You learn from your mistakes'''
```

## Use a hook for what patterns can't read

`slist` is why hooks exist. Skill names are right-aligned in fixed columns, can
hold spaces, and wrap. No regex can cut that, so the pattern only says which
lines are rows:

```toml plugins/avatar/character.toml
row = '''^{pager}?(?:\s*\d+:| {4}|Lv:\s+Name)'''
```

and the hook cuts by width:

```lua plugins/avatar/character.luau
local function columns(text)
    local names = {}
    local start, width = 1, FIRST_COLUMN
    while start <= #text do
        local name = trim(string.sub(text, start, start + width - 1))
        if name ~= "" then
            table.insert(names, name)
        end
        start += width
        width = COLUMN
    end
    return names
end
```

Watch the budget here. Every hook call gets a limited number of ticks, and each
step of a `string` pattern spends one. `read_practice` checks a row with a cheap
`string.find` first, because a long pattern backtracks over a row of plain words:

```lua plugins/avatar/character.luau
    -- Only a skill's cells go to the long pattern: on a row of words, such as
    -- what can be trained, its backtracking runs a hook out of budget.
    local line = Readers.body(event)
    if not string.find(line, "%d%s+%d+%s+%S+%s+%d") then
        return
    end
```

## Give the game's words

Declarative again. `commands.toml` holds what the client should say, such as how
to name an item in a command: "a pixie crossbow" gives "crossbow", and "a sash of
the captain's guard" gives "sash":

```toml plugins/avatar/commands.toml
[keywords]
skip = ["a", "an", "the", "some"]
phrase_ends = ["of", "that", "with"]
possessives = false
```

Its comment also records what the plugin leaves out: no `separator`, because
Avatar's spam check counts each command in a chain. A missing value is a
decision too, so write down why.

## Test it

Run the plugin over a fixture:

```sh
mudling plugin test plugins/avatar --log plugins/avatar/tests/fixtures/score.txt
```

It says what each pattern caught, with up to three lines and their captures.
For `score`, you'll see the start and all 19 rows:

```text
caught 1 character.reader.score.start#1 score.start
caught 19 character.reader.score.row#1 score.row
```

`caught 0` on a pattern you meant to hit is the first thing to look for. So is
a line of the table that no row caught.

Then turn a log into a test. Write a few lines of made-up text in
`tests/mytable.txt`, run `mudling plugin test --update`, read the
`tests/mytable.expected` it wrote, and commit both. `mudling plugin test` then
checks them, which is what registry CI will run. Avatar's own is
`plugins/avatar/tests/sample.txt`. Avatar goes further, with scrubbed fixtures
and Rust tests in `plugins/avatar/tests`, but your plugin doesn't need Rust.

Fixtures are made up, or the game's own tables with the player's details
changed. Real session logs hold other players' chat, so they stay on your
machine: `--log` reads one where it is, and nothing needs copying.
