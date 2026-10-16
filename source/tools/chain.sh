#!/bin/bash
# chain.sh R1 R2 R3 ...: warp to each in turn, 700 frames apart (from the previous room's running state), 3000 frames in the last
D="I:/1. Claude Code/Xenogears/project/GameData"
S=$(cat ng_sched.txt)
f=36101; pokes=""; jumps=""
for r in "$@"; do
  pokes="$pokes$f:8006F94E:$(printf %X $r)/2,$f:8006F954:0/2,$f:800B0064:1/1,"
  jumps="$jumps$f:8007954C:3,"
  f=$((f+700))
done
last=$((f-700+3000))
export XENOGEARS_CARD="I:/1. Claude Code/Xenogears/cmp/chain.mcd"; rm -f "$XENOGEARS_CARD"
export PROG_LOAD="ng36000.rs.state" PROG_POKE="${pokes%,}" PROG_JUMP="${jumps%,}"
../Xenogears-rs/target2/release/xenogears.exe --data "$D" --headless $last 0 chain "$S" 2>&1 | grep -E "^ran|error|stopped" | tr '\n' ' '
echo " [$*]"
