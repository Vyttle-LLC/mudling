-- Reads a table cut into cells of a fixed width, each "name  NN%". A name
-- too long for its cell ends its line alone, and its percent starts the
-- next line, under a blank name.
local WIDTH = 22
local wrapped = nil

local function trim(text)
    return string.match(text, "^%s*(.-)%s*$")
end

function read_row(event)
    local row = event.plain
    if not string.find(row, "%", 1, true) then
        wrapped = trim(row)
        return
    end
    for start = 1, #row, WIDTH do
        local name, percent = string.match(string.sub(row, start, start + WIDTH - 1), "^%s*(.-)%s+(%d+)%%%s*$")
        if percent then
            if name == "" then
                name, wrapped = wrapped, nil
            end
            if name then
                mud.store("abilities", name, tonumber(percent))
            end
        end
    end
end
