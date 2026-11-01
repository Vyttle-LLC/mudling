#!/usr/bin/env python3
"""Writes session.txt, the benchmark's fixture: run it here to regenerate.

Every word is made up, in the prototype's log format. The name isn't .log
because .gitignore keeps every .log out of the repo, as real logs must stay.
It only copies the shape measured on the prototype's
logs: about a third of the lines blank, a median of 25 characters, one GMCP
message per four lines of text with Char.Vitals the most common, and a move
every 33 lines. Text keeps ANSI color, as the wire does.
"""

import json
import random
from pathlib import Path

ROOMS = 60
MOVES = 180
RED, GREEN, CYAN, RESET = "\x1b[31m", "\x1b[1;32m", "\x1b[36m", "\x1b[0m"
EOL = "\n\r"  # as Diku-family servers end lines
EXITS = ["north", "east", "south", "west"]
PLACES = ["Quiet Glade", "Stone Bridge", "Old Mill", "Narrow Lane", "Mossy Hollow"]
FOES = ["a goblin", "a grey wolf", "the guard", "a cave rat"]
MOODS = ["shimmers faintly", "smells of rain", "hums softly", "lies very still"]
CHATTER = ["hello there", "anyone near the mill", "selling torches", "north is safe"]
WORDS = "the path leads past a low wall where moss grows thick and old stones lean".split()

random.seed(187)


def gmcp(package, payload):
    return f"\n[GMCP] {package} {json.dumps(payload)}\n"


def vitals(hp):
    fields = {name: str(hp + 3 * i) for i, name in enumerate(
        ["hp", "maxhp", "mana", "maxmana", "mv", "maxmv", "tnl", "gold", "align"])}
    fields.update(wimpy="0", position="standing", opponent="", enemy_health="")
    return gmcp("Char.Vitals", fields)


def prompt(hp):
    return f"\n\r<{hp}hp {hp + 40}mv>"


def sentence():
    return " ".join(random.choices(WORDS, k=random.randint(3, 12))).capitalize() + "."


def room_text(room):
    place = PLACES[room % len(PLACES)]
    lines = [f"{CYAN}{place} {room}{RESET}"]
    lines += [sentence() for _ in range(random.randint(2, 5))]
    lines.append(f"[Exits: {' '.join(EXITS)}]")
    lines.append("")
    lines.append(f"{GREEN}A torch {random.choice(MOODS)} here.{RESET}")
    return EOL.join(lines) + EOL


def room_info(room):
    exits = {EXITS[i][0]: (room + i + 1) % ROOMS for i in range(4)}
    place = PLACES[room % len(PLACES)]
    return gmcp("Room.Info", {"num": room, "name": f"{place} {room}", "zone": "Testland",
                              "environment": "field", "details": [], "exits": exits})


def fight(hp):
    foe = random.choice(FOES)
    return [f"{RED}You hit {foe}.{RESET}", f"{foe.capitalize()} misses you.",
            f"{foe.capitalize()} {random.choice(MOODS)}.", "", f"You feel {hp}% rested."]


def session():
    out = ["[connected]\nWelcome to Testland.\n\r\n[telnet] verb 251 option 201\n"]
    room, hp = 1, 400
    for _ in range(MOVES):
        direction = random.choice(EXITS)
        room = (room + EXITS.index(direction) + 1) % ROOMS
        out.append(f"\n[MAP] move {direction}\n")
        if random.random() < 0.1:
            out.append(f"You can't go that way.{EOL}")
            out.append(f"\n[MAP] move {direction}\n")
        out.append(room_text(room))
        if random.random() < 0.05:
            out.append(f"It is pitch black.{EOL}")
        out.append(room_info(room))
        out.append(gmcp("Room.Players", {"players": ["Ann", "Bo"]}))
        out.append(gmcp("Char.Items.List", {"location": "room", "items": []}))
        for _ in range(random.randint(2, 3)):
            hp = 300 + random.randrange(200)
            out.append(EOL.join(fight(hp)) + EOL)
            out.append(vitals(hp))
            out.append(prompt(hp))
            # The next GMCP is what shows the prompt is finished.
            out.append(gmcp("Char.Status", {"level": "10", "class": "Tester"}))
            out.append(EOL)
        if random.random() < 0.3:
            out.append(f"[gossip] Ann: '{random.choice(CHATTER)}'{EOL}")
        if random.random() < 0.1:
            out.append(f"You flee {random.choice(EXITS)}!{EOL}")
    out.append("\n[disconnected] closed by server\n")
    return "".join(out)


Path(__file__).with_name("session.txt").write_bytes(session().encode())
