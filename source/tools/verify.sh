#!/bin/bash
# verify.sh EXE [BASELINE_EXE]
#   1. the per-call check (decompiled function against the translated code, same machine state) over six scenarios
#   2. the same over a five-room warp chain
#   3. with BASELINE_EXE: the picture digests of 20000 boot frames of both builds must be identical
# run from this folder; prints PASS / FAIL lines and exits non-zero on a failure
EXE=${1:?usage: verify.sh EXE [BASELINE_EXE]}
BASE=$2
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE" || exit 2
D="$(cd .. && pwd)/project/GameData"
S=$(cat ng_sched.txt)
fail=0
export XENOGEARS_CARD="$HERE/verify.mcd"
export XG_GPU=0

echo "== per-call check, 6 scenarios"
while read st seed start end; do
  [ -z "$st" ] && continue
  rm -f "$XENOGEARS_CARD"
  out=$(XG_DEC_CHECK=60 PROG_LOAD=$st PROG_RANDOM=$seed:$start timeout 600 "$EXE" --data "$D" --headless $end 0 tr5/verify "$S" 2>&1)
  n=$(echo "$out" | grep -c "MISMATCH")
  ran=$(echo "$out" | grep -o "^ran [0-9]* frames" | grep -o "[0-9]*")
  if [ "$n" = "0" ] && [ "${ran:-0}" -ge "$end" ]; then echo "PASS $st seed $seed ($ran frames)"; else echo "FAIL $st seed $seed: $n mismatches, ran ${ran:-?} of $end frames"; fail=1; fi
done <<LIST
bt32700.rs.state 7 32700 33300
bt32700.rs.state 9 32700 33300
tr5/ng32590.state 3 32590 33300
tr5/ng32590.state 21 32590 33300
ng36000.rs.state 5 36000 36100
ng36000.rs.state 31 36000 36100
LIST

echo "== per-call check, 5-room warp chain"
f=37001; pokes=""; jumps=""
for r in 20 45 70 100 130; do
  pokes="$pokes$f:8006F94E:$(printf %X $r)/2,$f:8006F954:0/2,$f:800B0064:1/1,"
  jumps="$jumps$f:8007954C:3,"
  f=$((f+700))
done
last=$((f-700+3000))
rm -f "$XENOGEARS_CARD"
out=$(XG_DEC_CHECK=100 PROG_LOAD=tr5/ng32590.state PROG_POKE="${pokes%,}" PROG_JUMP="${jumps%,}" timeout 900 "$EXE" --data "$D" --headless $last 0 tr5/verify "$S" 2>&1)
n=$(echo "$out" | grep -c "MISMATCH")
ran=$(echo "$out" | grep -o "^ran [0-9]* frames" | grep -o "[0-9]*")
if [ "$n" = "0" ] && [ "${ran:-0}" -ge "$last" ]; then echo "PASS warp chain ($ran frames)"; else echo "FAIL warp chain: $n mismatches, ran ${ran:-?} of $last frames"; fail=1; fi

if [ -n "$BASE" ]; then
  echo "== picture digests, 20000 boot frames, against $BASE"
  for e in "$BASE" "$EXE"; do
    rm -f "$XENOGEARS_CARD"
    XG_DIGEST="tr5/verify_$(basename "$e").txt:100" XG_DIGEST_VRAM=1 timeout 600 "$e" --data "$D" --headless 20000 0 tr5/verify "$S" >/dev/null 2>&1
  done
  if cmp -s "tr5/verify_$(basename "$BASE").txt" "tr5/verify_$(basename "$EXE").txt"; then echo "PASS digests identical"; else echo "FAIL digests differ"; fail=1; fi
fi
[ $fail = 0 ] && echo "ALL PASS" || echo "FAILED"
exit $fail
