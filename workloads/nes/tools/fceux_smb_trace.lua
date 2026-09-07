-- SPDX-License-Identifier: AGPL-3.0-or-later

-- Trace SMB route transitions while FCEUX replays a checksum-matched movie.

FCEU.speedmode("maximum")

local output_path = assert(os.getenv("DISSONANCE_TRACE_OUT"), "DISSONANCE_TRACE_OUT is required")
local output = assert(io.open(output_path, "w"))

while not movie.active() do
    FCEU.frameadvance()
end

local previous_world = nil
local previous_level = nil
local maximum_progress = 0
local ready_logged = {}

while movie.framecount() < movie.length() do
    local frame = movie.framecount()
    local world = memory.readbyte(0x075f)
    local level = memory.readbyte(0x075c)
    local flag_task = memory.readbyte(0x0746)
    if flag_task == 0x05 and level > 0 then
        level = level - 1
    end
    local progress = memory.readbyte(0x071a) * 16 + math.floor(memory.readbyte(0x071c) / 16)
    local engine = memory.readbyte(0x000e)
    if world ~= previous_world or level ~= previous_level then
        output:write(string.format(
            "route\t%d\t%d\t%d\t%d\t%d\t%d",
            frame,
            world,
            level,
            progress,
            engine,
            flag_task
        ), "\n")
        output:flush()
        previous_world = world
        previous_level = level
        maximum_progress = progress
    elseif progress > maximum_progress then
        maximum_progress = progress
    end
    local ready_key = world * 4 + level
    if world < 8 and level < 4 and progress == 0 and (engine == 7 or engine == 8)
        and not ready_logged[ready_key] then
        output:write(string.format("ready\t%d\t%d\t%d\t%d\n", frame, world, level, engine))
        output:flush()
        ready_logged[ready_key] = true
    end
    FCEU.frameadvance()
end

output:write(string.format("complete\t%d\n", movie.framecount()))
output:flush()
output:close()
FCEU.pause()
while true do
    FCEU.frameadvance()
end
