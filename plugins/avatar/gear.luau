-- Gear: the eq table, where each item goes, identify and examine, and the
-- word a command should use for an item. See README.md for the stores.

-- `equipment`'s slot labels, by the short names in gear.toml.
local SLOTS = {
    ["used as light"] = "light",
    ["worn on finger"] = "finger",
    ["worn around neck"] = "neck",
    ["worn on body"] = "body",
    ["worn on head"] = "head",
    ["worn on legs"] = "legs",
    ["worn on feet"] = "feet",
    ["worn on hands"] = "hands",
    ["worn on arms"] = "arms",
    ["held in offhand"] = "offhand",
    ["worn about body"] = "about",
    ["worn about waist"] = "waist",
    ["worn on wrist"] = "wrist",
    ["wielded"] = "wield",
    ["held"] = "held",
}

-- Words that start an item's name or join its parts, never its noun.
local ARTICLES = { a = true, an = true, the = true, some = true }
local PHRASE_ENDS = { of = true, that = true, with = true }

-- The word a command should name an item by: the noun ending its first
-- phrase. "a pixie crossbow" → "crossbow", "a sash of the captain's
-- guard" → "sash". The game doesn't take possessives ("remove captain's"
-- fails), so one is never chosen while there's another word.
local function keyword(name)
    local words = {}
    for word in string.gmatch(Readers.plain(name), "[%a']+") do
        if PHRASE_ENDS[word] and #words > 0 then
            break
        end
        if not ARTICLES[word] then
            table.insert(words, word)
        end
    end
    for index = #words, 1, -1 do
        if not string.find(words[index], "'", 1, true) then
            return words[index]
        end
    end
    -- Only possessives: the name they're made of.
    local last = words[#words]
    return last and (string.match(last, "^(.-)'") or last)
end

local function note_item(write, item, slot)
    local plain = Readers.plain(item)
    if slot then
        write("gear", "slot." .. plain, slot)
    end
    local word = keyword(item)
    if word then
        write("gear", "keyword." .. plain, word)
    end
end

-- ---- eq ----

local finish_equipment = Readers.count_rows("equipment", "count")

function read_equipment(event)
    if event.name == "gear.equipment.start" then
        Readers.open("equipment", finish_equipment)
        return
    end
    local reader = Readers.get("equipment")
    local slot = SLOTS[event.captures.slot]
    if not reader or not slot then
        return
    end
    Readers.row(reader)
    local index = tostring(reader.rows)
    local item = event.captures.item
    if item == "<nothing>" then
        item = ""
    end
    Readers.write(reader, "equipment", index .. ".slot", slot)
    Readers.write(reader, "equipment", index .. ".item", item)
    if item ~= "" then
        note_item(function(...)
            Readers.write(reader, ...)
        end, item, slot)
    end
end

-- ---- wear ----

-- Where a wear message says an item went, by its slot's short name:
-- "... around your neck." → neck, "... about your body." → about.
local function slot_worn(event)
    local on, part = event.captures.on, event.captures.part
    if part then
        if on == "about" and part == "body" then
            return "about"
        end
        return part
    end
    local line = Readers.body(event)
    if string.find(line, "You hold ", 1, true) then
        return "held"
    elseif string.find(line, "You light ", 1, true) then
        return "light"
    end
    return "wield"
end

function on_wore(event)
    note_item(mud.store, event.captures.item, slot_worn(event))
end

-- ---- identify ----

-- The GMCP Char.Status key for each stat identify names.
local STATS = {
    strength = "str",
    intelligence = "int",
    wisdom = "wis",
    dexterity = "dex",
    constitution = "con",
    ["hit roll"] = "hitroll",
    hitroll = "hitroll",
    ["damage roll"] = "damroll",
    damroll = "damroll",
    ["armor class"] = "ac",
    ac = "ac",
    hp = "health_max",
    ["hit points"] = "health_max",
    mana = "mana_max",
    moves = "movement_max",
    move = "movement_max",
    movement = "movement_max",
}

local function add(totals, key, amount)
    totals[key] = (totals[key] or 0) + amount
end

local function finish_identify(reader)
    if not reader.keywords then
        -- Named, but no object followed.
        return
    end
    local prefix = reader.keywords .. "."
    local write = function(key, value)
        Readers.write(reader, "identify", prefix .. key, value)
    end
    if reader.item then
        write("name", reader.item)
        Readers.write(reader, "gear", "keywords." .. Readers.plain(reader.item), reader.keywords)
    end
    write("type", reader.type)
    write("flags", reader.flags)
    if reader.level then
        write("level", reader.level)
    end
    if reader.quality then
        write("quality", reader.quality)
    end
    for stat, amount in reader.base do
        write("base." .. stat, amount)
    end
    for spell, totals in reader.added do
        for stat, amount in totals do
            write("added." .. spell .. "." .. stat, amount)
        end
    end
    Readers.write(reader, "identify", "read", reader.keywords)
end

function read_identify(event)
    local captures = event.captures
    if event.name == "gear.identify.named" then
        -- A shop names the item on its own line, before the object.
        Readers.open("identify", finish_identify).item = captures.item
        return
    end
    if event.name == "gear.identify.object" then
        local reader = Readers.get("identify")
        if not reader or reader.keywords then
            -- The spell's identify, or the next of several.
            reader = Readers.open("identify", finish_identify)
        end
        reader.keywords = string.match(captures.keywords, "^%s*(.-)%s*$")
        reader.type = captures.type
        -- "glow magic"; a plain item says "none".
        local flags = {}
        for flag in string.gmatch(captures.flags or "", "%S+") do
            if flag ~= "none" then
                table.insert(flags, flag)
            end
        end
        reader.flags = table.concat(flags, " ")
        reader.base, reader.added = {}, {}
        return
    end
    local reader = Readers.get("identify")
    if not reader or not reader.keywords then
        return
    end
    Readers.row(reader)
    if captures.level then
        reader.level = tonumber(captures.level)
    elseif captures.quality then
        reader.quality = captures.quality
    elseif captures.armor then
        -- Armor counts down on the character: armor 1 is AC -1.
        add(reader.base, "ac", -tonumber(captures.armor))
    elseif captures.spell then
        local stat = STATS[string.lower(captures.stat)]
        if stat then
            local spell = string.match(captures.spell, "^%s*(.-)%s*$")
            reader.added[spell] = reader.added[spell] or {}
            add(reader.added[spell], stat, tonumber(captures.amount))
        end
    elseif captures.stat then
        local stat = STATS[string.lower(captures.stat)]
        if stat then
            add(reader.base, stat, tonumber(captures.amount))
        end
    end
end

-- ---- examine ----

-- Where `examine` says an item goes: "worn around the neck" → neck.
local function slot_meant(meant)
    local words = {}
    for word in string.gmatch(meant, "%S+") do
        if word ~= "the" and word ~= "a" and word ~= "your" then
            table.insert(words, word)
        end
    end
    local phrase = table.concat(words, " ")
    if string.sub(phrase, 1, 4) == "held" then
        return "held"
    elseif string.sub(phrase, 1, 7) == "used as" then
        return "light"
    end
    return SLOTS[phrase]
end

function read_examine(event)
    if event.name == "gear.examine.item" then
        Readers.open("examine").item = event.captures.item
        return
    end
    local reader = Readers.get("examine")
    local slot = reader and slot_meant(event.captures.meant)
    if slot then
        Readers.row(reader)
        Readers.write(reader, "gear", "slot." .. Readers.plain(reader.item), slot)
    end
end
