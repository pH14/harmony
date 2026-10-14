# SPDX-License-Identifier: AGPL-3.0-or-later
(.bugs // [])
| to_entries
| map(select(
    .value.confirmed == true
    and ((.value.violations // []) | index($assertion)) != null
    and ((.value.sometimes // []) | index($evidence)) != null
    and (.value.replay != null)
    and (.value.replay.bug == true)
    and (((.value.replay.violations // []) | index($assertion)) != null)
    and (((.value.replay.sometimes // []) | index($evidence)) != null)
    and (.value.replay.actions_applied == (.value.actions | length))
    and (.value.replay.guest_horizons == (.value.replay.actions_applied + .value.replay.settle_actions))
    and (.value.replay.settle_ticks >= .value.replay.settle_actions)
))
| (.[0] // empty)
| .key + 1
