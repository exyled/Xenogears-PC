#!/bin/bash
# hop.sh P R : warp to P, then (from P's running state) to R; 3000 frames in R
P=$1; R=$2
D="I:/1. Claude Code/Xenogears/project/GameData"
S=$(cat ng_sched.txt)
H1=36101; H2=37101
poke() { printf "%d:8006F94E:%X/2,%d:8006F954:0/2,%d:800B0064:1/1" $1 $2 $1 $1; }
export XENOGEARS_CARD="I:/1. Claude Code/Xenogears/cmp/hop_${P}_${R}.mcd"; rm -f "$XENOGEARS_CARD"
export PROG_LOAD="ng36000.rs.state"
export PROG_POKE="$(poke $H1 $P),$(poke $H2 $R)"
export PROG_JUMP="$H1:8007954C:3,$H2:8007954C:3"
../Xenogears-rs/target2/release/xenogears.exe --data "$D" --headless 40200 0 hop_${P}_${R} "$S" 2>&1 | grep -E "^ran|error|stopped" | tr '\n' ' '
echo " [$P -> $R]"
