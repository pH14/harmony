-- SPDX-License-Identifier: AGPL-3.0-or-later
local fiber = require('fiber')

-- Only the leader can create the space, and leadership moves, so every
-- instance keeps trying until the space exists.
fiber.create(function()
    while box.space.kv == nil or box.space.kv.index.pk == nil do
        if not box.info.ro then
            pcall(function()
                box.schema.space.create('kv', {
                    is_sync = true,
                    if_not_exists = true,
                    format = {{'k', 'string'}, {'v', 'string'}},
                })
                box.space.kv:create_index('pk', {parts = {'k'}, if_not_exists = true})
            end)
        end
        fiber.sleep(0.1)
    end
end)

function harness_leader()
    return box.info.election.leader_name or ''
end

function harness_write(k, v)
    box.space.kv:replace({k, v})
    return true
end

function harness_cas(k, old, new)
    box.begin()
    local t = box.space.kv:get(k)
    if t == nil or t[2] ~= old then
        box.rollback()
        return false
    end
    box.space.kv:replace({k, new})
    box.commit()
    return true
end

function harness_read(k)
    box.begin({txn_isolation = 'linearizable'})
    local t = box.space.kv:get(k)
    box.commit()
    if t == nil then
        return {false}
    end
    return {true, t[2]}
end

-- read-confirmed returns only transactions the quorum confirmed, from this
-- instance's own copy.
function harness_local(keys)
    box.begin({txn_isolation = 'read-confirmed'})
    local out = {}
    for i, k in ipairs(keys) do
        local t = box.space.kv:get(k)
        if t == nil then
            out[i] = {k, false}
        else
            out[i] = {k, true, t[2]}
        end
    end
    box.commit()
    return out
end
