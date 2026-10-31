-- Counts hits and reads the rest level, as a game's status hooks do.
local hits = 0

function on_hit(event)
    hits = hits + 1
    mud.store("fight", "hits", hits)
end

function on_rested(event)
    local percent = string.match(event.plain, "(%d+)%%")
    if percent then
        mud.store("fight", "rested", tonumber(percent))
    end
end
