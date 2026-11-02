-- Hooks are for what a pattern alone can't read. Each runs only on the lines
-- its pattern matched.

function on_welcome(event)
	mud.store("character", "name", event.captures.name)
end
