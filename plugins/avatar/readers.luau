-- What Avatar's table hooks share. The host frames each table; a hook here
-- collects its rows as store writes, and the table's end sends them
-- together. A table cut short sends nothing; nor does a start with no rows
-- after it, which was some other line.

local reading = nil

Readers = {}

-- Follows the table an event belongs to: `name`'s, whose `finish`, if
-- given, runs at its end, before the writes go out. Returns the event's kind
-- (start, row or gap) and the table's reader, or nothing once it's over. One
-- more start before any row is the same table. The event queue can drop
-- events, so what's open may not be `name`'s; a stray row is ignored.
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
    if not open then
        return nil
    end
    if kind == "gap" then
        reading.after_row = false
    end
    return kind, reading
end

-- A `finish` that writes how many rows the table had to `store`'s `key`,
-- the write that ends the table's batch.
function Readers.count_rows(store, key)
    return function(reader)
        Readers.write(reader, store, key, reader.rows)
    end
end

-- Queues a write to send when `reader`'s table ends.
function Readers.write(reader, store, key, value)
    table.insert(reader.writes, { store, key, value })
end

function Readers.row(reader)
    reader.rows += 1
end

-- Ends the open table, sending its writes.
function Readers.finish()
    local reader = reading
    reading = nil
    if not reader or reader.rows == 0 then
        return
    end
    if reader.finish then
        reader.finish(reader)
    end
    for _, write in reader.writes do
        mud.store(write[1], write[2], write[3])
    end
end

-- The text after a pager prompt glued to the front of a line.
function Readers.body(event)
    return string.match(event.plain, "%[C%]ontinue: ?(.*)$") or event.plain
end

-- An item's name as other lines give it, for keys: without tags such as
-- "(Glowing)", spaces collapsed, lowercase.
function Readers.plain(name)
    local untagged = string.gsub(name, "%b()", "")
    local words = {}
    for word in string.gmatch(untagged, "%S+") do
        table.insert(words, word)
    end
    return string.lower(table.concat(words, " "))
end
