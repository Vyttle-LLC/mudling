# Accessibility

PRD §8 asks for an accessible client: every control has a name a screen reader
says, the HUD reads as text, and the chat is announced line by line. This file
is the audit of every control MUDling draws, what checks it, and what still
needs a person with a screen reader.

```sh
cargo run --release -p mudling-desktop -- --synthetic --ax-check
```

`--ax-check` (macOS) runs the window off screen, never taking focus, walks it
through 56 stops (every panel, the HUD in each place, every settings section,
page and question it can reach without a real game) and prints a JSON report.
It exits 1 if a check fails. It checks two ways:

- **Named checks** look for given controls and text by name in the tree macOS
  gives VoiceOver, and for where focus lands.
- **The sweep** reads gpui's AccessKit tree, the one NVDA and Orca get, at
  every stop. It fails on a control with no name; one the keyboard can neither
  reach with Tab nor name a key for; a tab, switch, checkbox or radio button
  that doesn't say whether it's chosen or on; something clicked that isn't a
  control; an unnamed tab list, toolbar, menu, radio group, dialog or meter;
  and a meter with no reading.

## Counts

| | |
|---|---|
| Stops | 56 |
| Elements, rows folded together (the inventory below) | 167 |
| Controls seen in the audit's tree dumps, counting each row's copy (48 stops, before the update card) | 1,127 |
| Distinct controls in those dumps, by role and name | 315 |
| Gaps found | 18 |
| Gaps fixed | 18 |
| Checks in `--ax-check`, before and after (with the update card's two) | 82 and 102 |

## Gaps found and fixed

| Gap | Fix |
|---|---|
| The trigger, alias and Logs switches were all named "…, on", so an off switch said "on" | Switches are named for what they turn on; on or off is their state |
| Plugin switches said on or off twice, in the name and the state | Named for the plugin alone |
| Check for updates, the same | Named "Check for updates" |
| Where to today?'s update card offered a bare "Restart" | Named "Restart to update", as in Settings |
| HUD meters read "3,120/4,300" | They read "3,120 of 4,300" |
| The HUD's top strip drew the opponent as text no screen reader could reach | It's a label, read with its health |
| Tab and Shift-Tab skipped the hotbar: its chips and Add a chip held focus handles that weren't tab stops | They are tab stops, between the left panel and the command bar |
| Collapse the left panel and Collapse the right panel took no focus | Tab stops, as the panel tabs beside them are |
| A panel shown as a rail had buttons the keyboard couldn't reach | Tab stops |
| Both panels' tab lists had no name | Named "Panels" |
| Send took no focus and named no key | Says Enter; it stays out of the Tab order, where Tab completes a word |
| Pin to the hotbar, the same | Says ⌘P (Ctrl+P elsewhere) |
| Back to live took no focus and named no key | Says Escape |
| A dimmed control (Walk with nowhere to go, Practice while locked, Wear and New set without a character, Save on an unfinished set, Install while installing, Add while checking, Refresh offline, a locked hotbar chip) only said why in its description | Also marked unavailable, so a screen reader says "dimmed" |
| Shortcuts read symbols: "⌘L", "↑  ↓", and "End  Esc" for either key | Words: "Command+L", "Up Arrow or Down Arrow", "End or Escape" |
| Removing a server asked with a bare "Keep" | "Keep Sample game" |
| Removing a password, the same | "Keep the password for Sample on mud.example" |
| `--ax-check` never visited the HUD's strip, rails, a closed connection, the hotbar's editor, the session menu, Servers & characters, Triggers & aliases and its forms, Shortcuts, or the remove questions | It does now, with the sweep and a named check at each |

## Inventory

**Checked** says what in `--ax-check` covers the control: *named* (a check
finds it by name, value or state), *focus* (a check of where the keyboard
lands on it), *sweep* (only the sweep: its name, keyboard path and state). A
control with several rows, one per item or rule, is one line here.

### Main window

| Element | Role | Name | Checked |
|---|---|---|---|
| Left panel | Button, toggled | Left panel | named |
| Session | Button, expanded | Session: *state* | named |
| Search | Button, toggled | Search | named |
| Right panel | Button, toggled | Right panel | named |
| Settings | Button | Settings | named |
| Session menu | Menu | Sessions | named |
| A session in the menu | MenuItem, selected | *its name*, described by its state | named |
| Chat | Log | Chat | named |
| A chat line | StaticText | *the line* | named |
| New lines | Status (live) | *the lines* | named |
| Chat history, while split | Group | Chat history | named |
| Back to live | Button, key Escape | Back to live · *n* new lines | named |
| Find in the scrollback | TextInput | Find in the scrollback | focus |
| Next result, Previous result, Close find | Button, keys Enter, Shift+Enter, Escape | Next result, Previous result, Close find | sweep |
| Find's count | Status (live) | *n* results | named |
| Plugin suggestion | Label | *suggestion* | sweep |
| Connection ended | Alert | The connection failed: *why* | named |
| Turn on and reconnect, Reconnect, All servers | Button | Turn on and reconnect, Reconnect, All servers | named |
| Command bar | TextInput | Command | focus |
| Send | Button, key Enter | Send | sweep |
| Pin to the hotbar | Button, key ⌘P | Pin to the hotbar | named |
| Alias preview | Label | *expansion* | sweep |
| Hotbar | Toolbar | Hotbar | named |
| A chip | Button, n of m | *its command*, with its keys | named, focus |
| Add a chip | Button | Add a chip | named, focus |
| Hotbar editor | TextInput | Hotbar command | named |

### HUD

| Element | Role | Name | Checked |
|---|---|---|---|
| The row (attached, top strip) | Group | Status | sweep |
| Who | Label | *name · level · position* | sweep |
| Hit points, Mana, Moves | Meter | Hit points, Mana, Moves; value "*n* of *max*" | named |
| Show or hide status details | Button, expanded, key ⌘J | Show status details / Hide status details | named |
| Status details | Group | Status details | sweep |
| Opponent | Meter | *opponent*; value *health* | sweep |
| Next level | Meter | Next level; value "*n* to go" | named |
| Affects | Group of Labels | Affects | sweep |
| Floating card | Group, focusable, arrows move it | Status card. The arrow keys move it. | focus |
| Opponent on the strip and card | Label | *opponent · health* | named |
| Hidden | nothing drawn | | sweep |

### Panels

| Element | Role | Name | Checked |
|---|---|---|---|
| Left panel, Right panel | Group | Left panel, Right panel | sweep |
| Panel tabs | TabList | Panels | sweep |
| Map, Find, Room, Character, Gear, Skills, Notes | Tab, selected | *panel* | named (Notes), sweep |
| Collapse the left panel, Collapse the right panel | Button | *same* | sweep |
| A rail's buttons | Button | *panel* | named |

### Map

| Element | Role | Name | Checked |
|---|---|---|---|
| Duplicates line | Status (live) | *n* rooms look like duplicates | named |
| Review duplicates | Button | Review duplicates | named |
| Review | Group | Skip this pair, Not the same, Join these rooms, Close Review | named, focus |
| Both copies, side by side over the map | Group | First copy…, Second copy… | named |
| Pick a room to join | Button, toggled | Pick *room* to join | named |
| Picked | Status (live) | *n* rooms picked | named |
| Join the rooms picked, Clear the rooms picked | Button | *same* | named |
| Forget room | Button | Forget room *room* | named, focus |
| Forget's question | Alert with Buttons | Keep *room*, Forget *room* | named, focus |
| Add a note here | Button | Add a note here | sweep |
| Map history | Group, List | Map history, Saved versions | named |
| Restore | Button | Restore the map from *when, rooms* | named, focus |
| Restore's question | Alert with Button | Keep this map | named, focus |

### Find panel

| Element | Role | Name | Checked |
|---|---|---|---|
| Find on the map | TextInput | Find on the map | focus |
| All, Rooms, Mobs, Items | Tab, selected | *same* | named |
| A result | ListItem | *name, marks, place · steps* | named |
| Walk | Button, unavailable when it goes nowhere | Walk to *name* | named |
| Edit the note | Button | Edit the note | sweep |
| Count | Status (live) | *n* results | named |
| Why the list is empty | Label | *why* | named |

### Room panel

| Element | Role | Name | Checked |
|---|---|---|---|
| Room name, zone | Heading, Label | *room*, *zone* | sweep |
| Zoom in, Zoom out | Button | *same* | sweep |
| Exits | Button | Go *direction* / Open the door and go *direction* | named |
| Set a room mark | Button, toggled | Set *mark* | sweep |
| Look at, Kill | Button | Look at *mob*, Kill *mob* | sweep |
| Mark a mob | Button, toggled | Mark *mob* *mark* / Clear *mark* on *mob* | named |
| Get | Button | Get *item* | named |
| Edit the note | Button | Edit the note | sweep |

### Character, Gear, Skills, Notes

| Element | Role | Name | Checked |
|---|---|---|---|
| Character | Group | Character | named |
| Refresh the character | Button, unavailable offline | Refresh the character | named |
| Attributes, vitals | List of ListItems | *heading: value* | sweep |
| Level progress | ProgressIndicator | Level *n* → *n+1*, *xp* to go | sweep |
| Gear | Group | Gear | sweep |
| Worn, Carrying | Tab, selected | Worn, Carrying · *n* | named |
| Wear, Hold, Remove | Button, unavailable while locked | *action* *item* | named |
| More about | Button, expanded | More about *item* | named |
| A carried row | ListItem | *item, flags, level lock, stats → change* | named |
| Items named in chat | List of focusable ListItems | *item: tip* | named |
| Gear sets | Heading | Gear sets | named |
| A set | ListItem with Buttons | *set: pieces · container*; Wear *set*, Edit *set* | named |
| New set | Button | New set | named |
| Set name | TextInput | Set name | named |
| Container | Button, expanded | What comes off goes in: *container* | named |
| Exact copy | CheckBox | Exact copy of *item* | named |
| Words for, Drop | Button | Words for *item*, Drop *item* | named |
| Add a piece | Button | Add *item*, *where* | sweep |
| Use what I'm wearing now, Delete set, Cancel, Save | Button | *same* | named |
| What Wear would send | Group, one stop | Wearing it now would send: … | named |
| Skills | Group | Skills | sweep |
| Spells, Skills | Tab, selected | *same* | named |
| Filter skills | TextInput | Filter skills | sweep |
| Practice | Button, unavailable while locked | Practice *skill* | named |
| Notes | Group | Notes | named |
| Note | TextInput | Note | named |
| Done | Button | Done | sweep |
| A note | ListItem with Buttons | Go to *room*, Walk to *room* | named |
| Edit the notepad | Button | Edit the notepad | named |

### Settings

| Element | Role | Name | Checked |
|---|---|---|---|
| Settings | Dialog, modal | Settings | named |
| Sections | TabList | Settings sections | sweep |
| Each section | Tab, selected, with its problems described | *section* | named, focus |
| Close settings | Button | Close settings | named |
| The page | TabPanel | *page title* | sweep |
| **Appearance:** Theme, chat font, HUD placement | RadioGroup of RadioButtons | Theme, HUD placement and the chat font's group; System, Light, Dark; *fonts*; On the command bar, Floating card, Top strip, Hidden | named (Theme, HUD placement, Floating card), sweep |
| Smaller chat text, Larger chat text | Button | *same* | named |
| Show prompt lines | CheckBox | Show prompt lines | named |
| **Servers & characters:** Plugins | Button | Plugins for *server* | named |
| Remove a server | Button | Remove *server* | named |
| Its question | Alert with Buttons | Keep *server*, Remove *server* | named |
| **A server's plugins:** Back | Button | Back | named |
| Turn on a suggested plugin | Button | Turn on *plugin* | named |
| A plugin's switch | Switch | *plugin* | named |
| **Plugins:** Browse directory, Inspector | Button | *same* | named |
| A plugin's switch | Switch | *plugin* | named |
| Take back, Review | Button | Take back: *what*, *plugin*; Review: *why*, *plugin* | named |
| Remove a plugin | Button | Remove *plugin* | named |
| Turned off for overrunning | Alert with Buttons | Details (expanded), Turn on again | named |
| **Inspector:** Back, Clear | Button | *same* | named, focus |
| A line | ListItem | *line, who claimed it, time* | named |
| **Directory:** Back, Add from a link or folder | Button | *same* | named, sweep |
| Search plugins | TextInput | Search plugins | named |
| Game filter | RadioGroup | All games, *games*, Updates (*n*) | named |
| Install, Update | Button, unavailable while installing | Install *plugin*, Update *plugin* | named |
| **Add a plugin:** Git link | TextInput | Git link | named |
| Add the plugin at this link, Choose a plugin folder | Button, unavailable while checking | *same* | named |
| Remove an added plugin | Button | Remove *plugin* | named |
| **Approval, remove, trust, install** | AlertDialog | *question* | named |
| Its asks | List | send commands to the game… | named |
| A plugin that only asks for GMCP data | the same rows and asks | "Asks the game for data" in place of sending commands | unit tests (`settings::plugins`); the check's sample plugins all send commands |
| Cancel, Allow / Remove / Trust / Install | Button | *same* | named, focus |
| **Triggers & aliases:** a rule's switch | Switch | *pattern or alias* | named |
| Edit, Remove | Button | Edit *rule*, Remove *rule* | named |
| New trigger, New alias, New chip | Button | *same* | named |
| Earlier, Later | Button | Earlier *chip*, Later *chip* | named |
| Trigger form | TextInput, RadioGroups, Buttons | What the line matches; Text, Regular expression; *colors*; Show the line, Hide the line; *sounds*; All characters, This server, This character; Save trigger, Cancel | named |
| Alias form | TextInput, Button | Alias name, What it stands for, Save alias | named |
| Chip form | TextInput, Button | Hotbar command, Save chip | named |
| **Shortcuts:** a shortcut | Label | *what: keys in words* | named |
| **Logs:** Keep debug logs, Keep chat logs for *server* | Switch | *same* | named |
| Clear chat logs | Button | Clear chat logs | named |
| Pin, Export a log | Button, Pin toggled | Pin chat log of *server, when*; Export … | named |
| **Data & sync:** Sync now, Hide history, Stop syncing | Button | *same* | named |
| Update card, on top of the sheet | Group, Heading | MUDling *version* is ready | named |
| Its notes | List of ListItems | What's new; *each change* | named |
| Release notes, Later, Restart to update | Button | *same* | named |
| Settings, with an update waiting | Button | Settings, MUDling *version* is ready | named |
| Check for updates | Switch | Check for updates | named |
| Check now | Button | Check now | named |
| When it checks, last checked | Label | *words* | named |
| Restart's question, in place of the sheet | AlertDialog with Heading, Buttons | Restart to update?; *what restarting does*; Cancel, Restart | named, focus |
| An update that didn't go in, at the top of every section | Group with Alert | *title*; *why*, You're still on *version* | named |
| Its OK | Button | OK | named |
| **Vault:** Reveal, Change, Remove | Button | Reveal / Change / Remove the password for *character on server* | named |
| Its question | Buttons | Keep the password for…, Remove the password for… | named |
| Add a password | Button | Add a password | named |
| Server, Character, Password | TextInput, PasswordInput | *same*; the password reaches no node | named |
| Lock the vault after | RadioGroup | 5 minutes, 15 minutes, 1 hour, Never | named |
| Keep a copy of the key on this device | Switch | *same* | sweep |
| Lock now, Change master password, Enter the old master password | Button | *same* | named |

### Where to today?

| Element | Role | Name | Checked |
|---|---|---|---|
| A saved server's card | Group with Button | Connect to *server* | named, focus |
| Add a server | Button | Add a server | named |
| Quick connect | TextInput, Button | Quick connect, Connect | named |
| Add form | TextInput, Buttons | Server address, Save, Cancel | named, focus |
| Settings | Button | Settings | named |
| Update card, with an update ready | Group, Heading, Label | MUDling *version* is ready; Restart now, or it goes in when you quit. | named |
| Its Later and Restart | Button | Later, Restart to update | named |

### What the check doesn't reach

These need state the check can't make without a real game, vault or repo, so
neither the named checks nor the sweep visit them. Their controls come from
the same helpers as the ones it does visit (`text_button`, `control`, the
masked field), so a name or state lost in a helper shows up elsewhere, but a
label written wrong for one of these alone wouldn't.

- **Vault** with no vault yet (the master password, typed twice, and Create
  vault), locked (the master password, Unlock, Unlock on this device, Use the
  recovery key, Unlock with the recovery key), the recovery key shown (the
  key, Copy the recovery key, I've saved it), and two passwords for one
  character (the conflict's buttons).
- **Data & sync** before a repo is chosen (Choose a folder) and asking before
  it stops (Keep syncing, Stop syncing).
- **Where to today?** with nothing saved yet, and its session menu.
- **Directory** refused, and the approval for an install or a copy to trust
  (the same dialog as Remove, with other words).

## Keyboard

Every action has a key path. What the sweep can't see is order, so here is
what a Tab walk through each page finds.

- **Main window:** Shift-Tab and Tab go round title bar → left panel →
  hotbar → command bar → right panel. Tab in the command bar completes a
  word, so the way out of it is Shift-Tab (checked: it lands on Add a chip),
  and ⌘L comes back. ⌘1 and ⌘2 open and close the panels, ⌘3 goes to the Room
  panel, ⌘4 to the hotbar, ⌘5 to Find, ⌘6 to Notes, ⌘J to the HUD. Enter sends,
  ⌘P pins, Escape or End goes back to live: the buttons for those say so and
  stay out of the Tab order.
- **Hotbar:** left and right move along it, Shift and an arrow move a chip,
  E edits, Delete removes, Escape goes back to the command bar.
- **Session menu:** up and down move, Tab and Escape close it.
- **Floating HUD card:** ⌘J brings the keyboard to it; the arrow keys move it;
  Escape goes back.
- **Settings:** the sheet keeps the keyboard. Tab goes round the section
  tabs, Close settings, then the page in the order it's drawn (Back first on a
  page that has one). A question (approval, remove) keeps Tab on its own
  buttons, starting on Cancel. Up, down, Page Up and Page Down scroll what has
  nothing to land on.
- **Map:** Review and the questions take focus on their first button and give
  it back where it came from; ⌘⇧J picks the room shown.

The map drawing itself is not something a screen reader can read room by
room. The Find panel, the Room panel and the Notes tab say what it shows,
and are the way round it by keyboard.

## What can't be checked here

`--ax-check` reads the trees screen readers are given; it can't hear what they
make of them. Before a public release (VYT-239 holds this gate):

- **VoiceOver on macOS, by hand.** That each new chat line is spoken once, in
  order, without the whole log being re-read; that a wrapped line is one
  utterance; that "dimmed", switch states, "n of m" and the meters' readings
  are spoken as the tree says; that the settings sheet traps the VoiceOver
  cursor as it does the keyboard; how the floating card and its arrow keys
  feel.
- **NVDA on Windows.** The same tree reaches UI Automation through AccessKit.
  CI builds and tests MUDling on Windows but never runs it with a screen
  reader, and the sweep only proves what the tree holds. Live regions, the
  modal sheet, toggled, expanded and unavailable states, key shortcuts and
  the Tab order all need hearing there.
- **Orca on Linux.** The same through AT-SPI, with the same gap. Each
  platform adapter maps AccessKit's properties its own way, so whether Orca
  says, for one, a key shortcut or a dimmed button needs hearing.
- **A screen-reader user's session.** Someone who plays MUDs with a screen
  reader, playing a real session: whether the chat keeps up at a real game's
  pace, whether the names here are the words they'd want, and what's missing
  that no checklist would think to ask.
