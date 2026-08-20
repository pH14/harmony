#!/bin/bash
set -u
cd /root/harmony-smb-goal/dissonance-v2
export HARMONY_SMB_ROM=/root/harmony-roms/smb.nes
/root/harmony-smb-goal/disk-gate.sh 30 || { echo "C116_SENTINEL: DONE"; exit 2; }
/root/.cargo/bin/cargo build --release 2>&1 | tail -2
echo "C116_BUILD_EXIT=$?"
rm -rf target/smb-completion/c116-conquest
nice -n 10 ./target/release/smb-campaign run \
  target/smb-completion/c115-conquest/archive-live.json \
  0x5eed_c034 12 50000 4096 msr1 \
  target/smb-completion/c116-conquest \
  --selector concentrated_recency_128 \
  --retention probe_at_admission_45_snapback_16 \
  --vocabulary down_ten_mask \
  --key frozen_room_x_16:3,1,208 \
  --waypoint waypoint_4_bucket_uniform:7,0,0,219,0,15 \
  --resume frontier_shortest \
  --replacement fewest_frames_in_level
echo "C116_RUN_EXIT=$?"
nice -n 10 ./target/release/smb-completion derive-ladder \
  target/smb-completion/c116-conquest/archive-live.json \
  target/smb-completion/c116-conquest/ladder.json > /dev/null 2>&1
echo "C116_LADDER_EXIT=$?"
nice -n 10 ./target/release/smb-completion census-frame-cost \
  target/smb-completion/c116-conquest/archive-live.json 7 0 \
  target/smb-completion/c116-conquest/frame-cost-81.json
echo "C116_FRAMECOST_EXIT=$?"
nice -n 10 ./target/release/smb-completion census-lineage-levels \
  target/smb-completion/c116-conquest/archive-live.json \
  target/smb-completion/c116-conquest/lineage-levels.json
echo "C116_LINEAGE_EXIT=$?"
echo "C116_SENTINEL: DONE"
