-- The character's tables: score, practice, slist and aff. Each read ends
-- with a `read` write naming how many rows it had; see README.md.

local function trim(text)
    return string.match(text, "^%s*(.-)%s*$")
end

-- ---- score ----

-- Score labels and the character keys they fill. The attributes' current
-- values come from their own rows, with the permanent value beside them.
local SCORE_FACTS = {
    Gold = "gold",
    Practices = "practice",
    Wimpy = "wimpy",
    Killed = "killed",
    Died = "died",
    Arrests = "arrests",
    ["In Bank"] = "bank",
    Incarnation = "incarnation",
}

local function read_score_row(reader, line)
    local write = function(key, value)
        Readers.write(reader, "character", key, value)
    end
    -- A hero's level carries its own, and a race can be two words: "Level
    -- 51(14) Kzinti Monk", "Level 4 Lesser Imp Monk    Exp/TNL: ...".
    local level, who = string.match(line, "^Level (%d+)%S* (.*)$")
    if level then
        who = string.gsub(who, "%s*Exp/TNL.*$", "")
        local race, class = string.match(who, "^(.-)%s+(%S+)%s*$")
        write("level", tonumber(level))
        write("race", race)
        write("class", class)
        return
    end
    local age = string.match(line, "^You are (%d+) years old")
    if age then
        write("age", tonumber(age))
        return
    end
    -- "Str    :    14 (17)     You have 110/110 hps."
    local stat, now, perm = string.match(line, "^(%a%a%a)%s*:%s*(%d+)%s*%((%d+)%)")
    if stat then
        local key = string.lower(stat)
        write(key, tonumber(now))
        write(key .. "_perm", tonumber(perm))
        return
    end
    -- "Gold   :        141     Gold Weight :          1     In Bank     :          0"
    for label, value in string.gmatch(line, "(%u[%a ]-)%s*:%s+(%S+)") do
        local key = SCORE_FACTS[trim(label)]
        if key then
            write(key, tonumber(value) or value)
        end
    end
end

local finish_score = Readers.count_rows("character", "score.read")

function read_score(event)
    local kind, reader = Readers.step(event, "score", finish_score)
    if kind == "start" then
        Readers.write(reader, "character", "title", event.captures.title)
    elseif kind == "row" then
        Readers.row(reader)
        read_score_row(reader, event.plain)
    end
end

-- ---- practice ----

local finish_practice = Readers.count_rows("skills", "practice.read")

function read_practice(event)
    local kind, reader = Readers.step(event, "practice", finish_practice)
    if kind == "start" then
        local sessions = event.captures.sessions
        if sessions then
            Readers.write(reader, "skills", "practice.sessions", tonumber(sessions))
        end
        return
    elseif kind ~= "row" then
        return
    end
    -- "dodge              90   4   -    0  kick                33   7   -    0"
    for name, learned, level, mana in string.gmatch(Readers.body(event), "([a-z][a-z' %-]-)%s+(%d+)%s+(%d+)%s+%S+%s+(%d+)") do
        name = trim(name)
        Readers.row(reader)
        Readers.write(reader, "skills", name .. ".learned", tonumber(learned))
        Readers.write(reader, "skills", name .. ".level", tonumber(level))
        Readers.write(reader, "skills", name .. ".mana", tonumber(mana))
    end
end

-- "You practice dodge with Ulric. Your current skill level is now 45."
function on_practiced(event)
    mud.store("skills", event.captures.skill .. ".learned", tonumber(event.captures.learned))
end

-- ---- slist ----

-- Names are right-aligned in fixed columns, 19 wide and then 18, and can
-- hold spaces, so the columns are cut by width.
local FIRST_COLUMN = 19
local COLUMN = 18

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

-- Whether `text` is cut into whole columns, each name ending at its
-- column's edge. Indented room text after a row isn't.
local function aligned(text)
    text = string.match(text, "^(.-)%s*$")
    local start, width = 1, FIRST_COLUMN
    while start <= #text do
        local cell = string.sub(text, start, start + width - 1)
        if #cell < width or (string.find(cell, "%S") and string.sub(cell, -1) == " ") then
            return false
        end
        start += width
        width = COLUMN
    end
    return #text > 0
end

local finish_slist = Readers.count_rows("skills", "slist.read")

function read_slist(event)
    local kind, reader = Readers.step(event, "slist", finish_slist)
    if kind ~= "row" then
        return
    end
    local line = Readers.body(event)
    local level, rest = string.match(line, "^%s*(%d+):(.*)$")
    if level then
        level = tonumber(level)
    elseif reader.after_row and string.sub(line, 1, 4) == "    " and aligned(string.sub(line, 5)) then
        -- A wrapped row continues the last one, without its level.
        level, rest = reader.level, string.sub(line, 5)
    else
        reader.after_row = false
        return
    end
    for _, name in columns(rest) do
        Readers.row(reader)
        Readers.write(reader, "skills", name .. ".level", level)
    end
    reader.level, reader.after_row = level, true
end

-- ---- aff ----

local finish_affects = Readers.count_rows("affects", "count")

function on_no_affects(event)
    mud.store("affects", "count", 0)
end

function read_affects(event)
    local kind, reader = Readers.step(event, "affects", finish_affects)
    if kind == "start" then
        reader.seen = {}
        return
    elseif kind ~= "row" then
        return
    end
    local name, detail = event.captures.name, event.captures.detail or ""
    -- The same spell cast twice is listed twice; it's on once.
    local entry = name .. "\n" .. detail
    if reader.seen[entry] then
        return
    end
    reader.seen[entry] = true
    Readers.row(reader)
    local index = tostring(reader.rows)
    Readers.write(reader, "affects", index .. ".name", name)
    if detail ~= "" then
        Readers.write(reader, "affects", index .. ".detail", detail)
    end
end
