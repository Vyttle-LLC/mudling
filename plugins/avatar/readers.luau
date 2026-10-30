-- Framing for the tables Avatar prints. One table is read at a time: its
-- start pattern opens a reader, its rows add store writes to it, and the
-- next prompt or the next table's start ends it, sending the writes
-- together. A table left at an unanswered pager (its line followed by a
-- prompt, with no row, blank or rule line between) was cut short and sends
-- nothing; nor does a start
-- with no rows after it, which was some other line.

local reading = nil

Readers = {}

-- Opens a reader called `name`, ending the one before. `finish`, if
-- given, runs at the end, before the writes go out.
function Readers.open(name, finish)
    Readers.finish()
    reading = { name = name, writes = {}, rows = 0, paged = false, finish = finish }
    return reading
end

-- The reader open, if it's `name`'s.
function Readers.get(name)
    if reading and reading.name == name then
        return reading
    end
    return nil
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

-- Counts a row read, which shows the pager was answered.
function Readers.row(reader)
    reader.rows += 1
    reader.paged = false
end

-- Ends the open table, sending its writes unless it was cut short.
function Readers.finish()
    local reader = reading
    reading = nil
    if not reader or reader.paged or reader.rows == 0 then
        return
    end
    if reader.finish then
        reader.finish(reader)
    end
    for _, write in reader.writes do
        mud.store(write[1], write[2], write[3])
    end
end

-- The text after a prompt or pager prompt glued to the front of a line.
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

function on_prompt(event)
    Readers.finish()
end

function on_pager(event)
    if reading then
        reading.paged = true
    end
end

-- A blank or rule line: the pager was answered, even by a last page with
-- no rows, and slist's wrapped rows have ended.
function on_blank(event)
    if reading then
        reading.paged = false
        reading.after_row = false
    end
end
