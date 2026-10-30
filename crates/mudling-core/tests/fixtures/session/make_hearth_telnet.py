#!/usr/bin/env python3
"""Writes hearth.telnet: a made-up session with the made-up game Hearth, as the
bytes a socket would read. Run it from this folder after changing the script.
No real session goes in fixtures: logs hold other players' chat."""

import json
import zlib

IAC, WILL, SB, SE, GA = 255, 251, 250, 240, 249
ECHO, MSSP, COMPRESS2, GMCP = 1, 70, 86, 201
MSSP_VAR, MSSP_VAL = 1, 2


def will(option):
    return bytes([IAC, WILL, option])


def gmcp(package, body):
    return bytes([IAC, SB, GMCP]) + f"{package} {json.dumps(body)}".encode() + bytes([IAC, SE])


def mssp(name):
    return (
        bytes([IAC, SB, MSSP, MSSP_VAR]) + b"NAME" + bytes([MSSP_VAL]) + name + bytes([IAC, SE])
    )


def prompt(hp, moves):
    return gmcp("Char.Vitals", {"hp": hp, "maxhp": 30, "mv": moves}) + (
        f"<{hp}hp {moves}mv> ".encode() + bytes([IAC, GA])
    )


plain = (
    will(GMCP)
    + will(MSSP)
    + will(COMPRESS2)
    + mssp(b"Hearth")
    + b"Welcome to \x1b[1;31mHearth\x1b[0m.\r\n"
    + bytes([IAC, SB, COMPRESS2, IAC, SE])
)

compressed = zlib.compress(
    gmcp("Room.Info", {"num": 1, "name": "A quiet road", "zone": "Meadow", "exits": {"n": 2}})
    + b"A quiet road\r\n"
    + prompt(20, 30)
    + b"Mira says 'welcome'\r\n"
    + b"Mira waves at you.\r\n"
    + b"You are now 45% in dodge.\r\n"
    + b"You wear a pink ring.\r\n"
    + prompt(20, 29)
    + gmcp("Room.Info", {"num": 2, "name": "A gate", "zone": "Meadow", "exits": {"s": 1}})
    + b"A gate\r\n"
    + b"A grey wolf lunges at you!\r\n"
    + prompt(11, 28)
)

with open("hearth.telnet", "wb") as fixture:
    fixture.write(plain + compressed)
