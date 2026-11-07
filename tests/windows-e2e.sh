#!/usr/bin/env bash
# End to end from WSL: real `clearway` client -> `clearway.exe relay` (interop) -> named pipe.
# Test instance only: exe copied to %LOCALAPPDATA%\clearway-test\bin, own state dir and pipe, --test (nothing on
# screen), stopped through the pipe at the end. The installed Clearway is never touched.
set -u
REPO=$(cd "$(dirname "$0")/.." && pwd)
LOCAL_W=$(cd /mnt/c && cmd.exe /d /c 'echo %LOCALAPPDATA%' 2>/dev/null | tr -d '\r')
TEST="$(wslpath "$LOCAL_W")/clearway-test"
EXE="$TEST/bin/clearway.exe"
EXE_W="$LOCAL_W"'\clearway-test\bin\clearway.exe'
PIPE=clearway-test
CW="$REPO/target/release/clearway"
export CLEARWAY_EXE="$EXE" CLEARWAY_PIPE="$PIPE" CLEARWAY_DIR="$TEST" CLEARWAY_LAUNCH_ARGS=--test CLEARWAY_SESSION=wsl-e2e
export LC_ALL=C.UTF-8
unset CLAUDE_PID CLAUDE_CODE_SESSION_ID CLEARWAY_LANG
failures=0
step() { printf '\n=== %s\n' "$*"; }
check() { if eval "$2"; then echo "OK    $1"; else echo "FAILED $1"; failures=$((failures + 1)); fi; }
ps_run() { (cd /mnt/c && powershell.exe -NoProfile -NonInteractive -Command "$1" | tr -d '\r'); }
# Only processes started from the test copy: an installed Clearway running alongside is neither counted nor stopped.
test_processes() { echo "Get-Process clearway -ErrorAction SilentlyContinue | Where-Object { \$_.Path -eq '$EXE_W' }"; }
instances() { ps_run "@($(test_processes)).Count"; }
quit() { [ -e "$EXE" ] && echo '{"method":"POST","path":"/quit","params":{},"body":null}' | (cd /mnt/c && "$EXE" relay --pipe "$PIPE") >/dev/null; sleep 2; }

quit 2>/dev/null
ps_run "$(test_processes) | Stop-Process -Force" >/dev/null 2>&1
mkdir -p "$TEST/bin"
rm -f "$TEST/state.json" "$TEST/clearway.log"
printf 'pipe = "%s"\nlegacy_pipe = false\nlanguage = "en"\n' "$PIPE" > "$TEST/config.toml"
cp "$REPO/target/x86_64-pc-windows-gnu/release/clearway.exe" "$EXE" || { echo "FAILED copy of the test exe"; exit 1; }

step "1. Relay without Clearway, then automatic launch by the client"
(cd /mnt/c && echo '{"method":"GET","path":"/status","params":{},"body":null}' | "$EXE" relay --pipe "$PIPE" >/dev/null 2>&1); code=$?
check "relay: code 69 when Clearway is not running" '[ $code = 69 ]'
s=$("$CW" status 2>&1)
echo "$s" | head -4
check "status through the relay after automatic launch" 'echo "$s" | grep -q "Clearway (epoch 1)"'
check "one test instance" '[ "$(instances)" = 1 ]'

step "2. Cost of a \"clearway status\" through the relay (5 calls)"
for i in 1 2 3 4 5; do /usr/bin/time -f "%e s" "$CW" status >/dev/null; done 2>&1

step "3. request, wait in the foreground, release"
free=$("$CW" status | awk '/Windows RAM +available/{for(i=1;i<=NF;i++) if($i=="free"){print $(i+1)}}')
A=$(awk -v l="$free" 'BEGIN{printf "%.1fG", l*0.7}')
B=$(awk -v l="$free" 'BEGIN{printf "%.1fG", l*0.5}')
echo "Free Windows RAM: $free GB -> A $A, B $B"
t1=$("$CW" request --side windows --ram "$A" "A (takes the Windows RAM)" 2>/dev/null)
t2=$("$CW" request --side windows --ram "$B" "B" 2>/dev/null)
"$CW" wait "$t2" --max 3s; code=$?
check "wait --max: code 75, place kept" '[ $code = 75 ] && "$CW" status | grep -qE "^#$t2 +waiting"'
( sleep 2; "$CW" release "$t1" >/dev/null ) &
"$CW" wait "$t2" --max 1m > "$TEST/wait.txt"; code=$?
cat "$TEST/wait.txt"
check "wait returns 0 on clearance" '[ $code = 0 ] && grep -q "CLEARED: ticket #$t2" "$TEST/wait.txt"'
"$CW" extend "$t2" 15m "extension test"; code=$?
check "extend" '[ $code = 0 ]'
"$CW" release "$t2" >/dev/null

step "4. run: exit code passed through, then --max and reattach"
"$CW" run --side windows --ram 1G "echo" -- sh -c 'echo hello; exit 5'; code=$?
check "run returns the command's exit code" '[ $code = 5 ]'
t3=$("$CW" request --side windows --ram "$A" "occupier" 2>/dev/null)
"$CW" run --side windows --ram "$B" --max 3s "patient" -- echo finally; code=$?
check "run --max: code 75" '[ $code = 75 ]'
( sleep 3; "$CW" release "$t3" >/dev/null ) &
output=$("$CW" run --side windows --ram "$B" --max 1m "patient" -- echo finally 2>&1); code=$?
echo "$output"
check "the same command takes its ticket back and runs" '[ $code = 0 ] && echo "$output" | grep -q "resumed" && echo "$output" | grep -q "^finally$"'

step "5. Clean stop through the pipe"
wait
quit
check "no test instance left" '[ "$(instances)" = 0 ]'
check "log: START" 'grep -q "START epoch 1 on pipe $PIPE" "$TEST/clearway.log"'
check "log: STOP" 'grep -q "STOP requested through the pipe" "$TEST/clearway.log"'

step "Log"
cat "$TEST/clearway.log"
echo
echo "Failures: $failures"
exit $failures
