# Brand fonts

Static cuts of the variable fonts in the Vyttle brand kit, bundled into
`mudling-desktop`. gpui draws a variable font at its default weight only, so
each weight the app uses is its own file.

| File | Source | Axes |
|---|---|---|
| `DMSans-Regular.ttf` | DM Sans variable | `wght` 400, `opsz` 14 |
| `DMSans-Medium.ttf` | DM Sans variable | `wght` 500, `opsz` 14 |
| `JetBrainsMono-SemiBold.ttf` | JetBrains Mono variable | `wght` 600 |

Cut with `fontTools.varLib.instancer`. Both fonts are under the SIL Open Font
License 1.1: see `OFL-DMSans.txt` and `OFL-JetBrainsMono.txt`.
