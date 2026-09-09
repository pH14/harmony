local f = assert(io.open(assert(os.getenv('HARMONY_OBSERVER_OUT')) .. '/clock.csv', 'w'))
f:write('call,movie_frame,emu_frame,mode\n')
assert(movie.load(assert(os.getenv('HARMONY_OBSERVER_MOVIE')), true))
for call = 0, 20 do
    f:write(string.format('%d,%d,%d,%s\n', call, movie.framecount(), emu.framecount(), tostring(movie.mode())))
    f:flush()
    if call < 20 then emu.frameadvance() end
end
f:close()
emu.exit()
