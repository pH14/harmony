-- Reporting-only positive control. No memory or controller writes, no search.
local out = assert(os.getenv('HARMONY_OBSERVER_OUT'))
local input = assert(os.getenv('HARMONY_OBSERVER_MOVIE'))
local frames = 120540
-- Fail closed: stop playback and request frontend exit on any checked error.
local function checked(condition, message)
    if condition then return condition end
    local f = io.open(out .. '/failure.txt', 'w')
    if f then f:write(tostring(message) .. '\nmovie_frame=' .. tostring(movie.framecount()) .. '\n'); f:close() end
    if movie.active() then movie.stop() end
    emu.exit()
    error(message or 'observer check failed')
end
local trace = checked(io.open(out .. '/relevant-frames.csv', 'w'))
local columns = {'frame', 'area', 'mode', 'door', 'room', 'loader', 'kraid', 'ridley'}
for slot = 0, 5 do
    for _, field in ipairs({'offset', 'status', 'type', 'special', 'hp', 'x', 'y', 'nametable'}) do
        columns[#columns + 1] = 'slot' .. slot .. '_' .. field
    end
end
trace:write(table.concat(columns, ',') .. '\n')
local rows, bytes, loader, agreement = 0, 0, 0, 0
local first_loader, first_kraid, first_ridley = -1, -1, -1
local first_active = -1
local last_kraid, last_ridley = 0, 0
local kraid_clear_seen, ridley_clear_seen = false, false
local function note_progress(frame, complete)
    local f = checked(io.open(out .. '/summary.json', 'w'))
    f:write(string.format('{"format":"metroid-public-observer-v1","complete":%s,"advanced_frames":%d,"movie_frames":%d,"guarded_loader_frames":%d,"loader_and_active_tag_frames":%d,"first_loader_frame":%d,"first_active_tag_frame":%d,"first_kraid_defeat_frame":%d,"first_ridley_defeat_frame":%d,"trace_rows":%d,"trace_bytes":%d}\n',
        tostring(complete), frame, frames, loader, agreement, first_loader,
        first_active, first_kraid, first_ridley, rows, bytes))
    f:close()
end
note_progress(0, false)
checked(movie.load(input, true), 'movie failed to load')
checked(movie.active() and movie.readonly() and movie.ispoweron(), 'power-on read-only movie required')
checked(movie.length() == frames and movie.framecount() == 0, 'unexpected movie horizon or starting frame')
checked(string.lower(rom.gethash('md5')) == 'b2d2d9ed68b3e5e0d29053ea525bd37c', 'ROM payload checksum differs')
emu.speedmode('maximum')
-- FCEUX 2.6.5 resumes a newly loaded Lua movie once at frame zero.
-- Clock diagnostic: calls 0,1,2..20 observed frames 0,0,1..19.
emu.frameadvance()
checked(movie.framecount() == 0, 'unexpected startup synchronization phase')
for frame = 1, frames do
    checked(movie.mode() == 'playback', 'movie ended before its fixed horizon')
    emu.frameadvance()
    checked(movie.framecount() == frame, 'movie clock skipped or repeated a frame')
    local read = memory.readbyte
    local area, mode, present = read(0x74), read(0x1e), read(0x6987)
    local kraid, ridley = read(0x687b), read(0x687c)
    local slots, tagged, active = {}, false, false
    for offset = 0, 0x50, 0x10 do
        local status, special = read(0x6af4 + offset), read(0x40f + offset)
        local tag = math.floor(special / 64) % 2 == 1
        tagged = tagged or tag
        active = active or (tag and status ~= 0)
        for _, value in ipairs({offset, status, read(0x6b02 + offset), special,
            read(0x40b + offset), read(0x401 + offset), read(0x400 + offset), read(0x6afb + offset)}) do
            slots[#slots + 1] = value
        end
    end
    local guarded = (area == 0x12 or area == 0x14) and mode == 3 and present == 1
    if guarded then
        loader = loader + 1
        if first_loader < 0 then first_loader = frame end
        if active then
            agreement = agreement + 1
            if first_active < 0 then first_active = frame end
        end
    end
    -- Require a live clear state before a rising defeat bit in the matching
    -- area. Power-on RAM contents are not evidence of defeating a boss.
    if mode == 3 and kraid % 2 == 0 then kraid_clear_seen = true end
    if mode == 3 and math.floor(ridley / 2) % 2 == 0 then ridley_clear_seen = true end
    if kraid_clear_seen and area == 0x12 and last_kraid % 2 == 0
        and kraid % 2 == 1 and first_kraid < 0 then first_kraid = frame end
    if ridley_clear_seen and area == 0x14 and math.floor(last_ridley / 2) % 2 == 0
        and math.floor(ridley / 2) % 2 == 1 and first_ridley < 0 then first_ridley = frame end
    -- Keep every frame in either boss area, including hit states that overwrite
    -- the special tag. This changes reporting only.
    if area == 0x12 or area == 0x14 or present ~= 0 or tagged
        or kraid ~= last_kraid or ridley ~= last_ridley then
        local fields = {frame, area, mode, read(0x56), read(0x5a), present, kraid, ridley}
        for _, value in ipairs(slots) do fields[#fields + 1] = value end
        local line = table.concat(fields, ',') .. '\n'
        bytes = bytes + #line
        checked(bytes <= 32 * 1024 * 1024, 'trace exceeded output cap')
        trace:write(line)
        rows = rows + 1
    end
    last_kraid, last_ridley = kraid, ridley
    if frame % 1000 == 0 then trace:flush(); note_progress(frame, false) end
end
trace:close()
note_progress(frames, true)
emu.exit()
