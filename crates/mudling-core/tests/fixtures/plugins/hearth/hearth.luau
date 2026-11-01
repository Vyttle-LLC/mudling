function on_skill(event)
    mud.store("skills", event.captures.skill, tonumber(event.captures.percent))
end

function on_wear(event)
    mud.store("gear", "worn." .. event.captures.item, true)
end

function on_wave(event)
    mud.send("wave " .. event.captures.who)
end
