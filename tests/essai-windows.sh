#!/usr/bin/env bash
# Bout en bout depuis WSL : vrai client `charge` -> `orchestrateur.exe relais` (interop) -> tube nommé.
# Instance de test en mode --test (rien à l'écran), arrêtée par le tube à la fin.
set -u
DEPOT=$(cd "$(dirname "$0")/.." && pwd)
LOCAL=$(wslpath "$(cd /mnt/c && cmd.exe /d /c 'echo %LOCALAPPDATA%' 2>/dev/null | tr -d '\r')")
EXE="$LOCAL/Programs/orchestrateur/orchestrateur.exe"
TEST="$LOCAL/orchestrateur-test"
CHARGE="$DEPOT/target/release/charge"
export CHARGE_EXE="$EXE" CHARGE_TUBE=orchestrateur-test CHARGE_DOSSIER="$TEST" CHARGE_ARGS_LANCEMENT=--test CHARGE_SESSION=essai-wsl
unset CLAUDE_PID CLAUDE_CODE_SESSION_ID
echecs=0
etape() { printf '\n=== %s\n' "$*"; }
verifie() { if eval "$2"; then echo "OK    $1"; else echo "ÉCHEC $1"; echecs=$((echecs + 1)); fi; }
instances() { (cd /mnt/c && powershell.exe -NoProfile -Command '@(Get-Process orchestrateur -ErrorAction SilentlyContinue).Count' | tr -d '\r'); }
quitter() { echo '{"methode":"POST","chemin":"/quitter"}' | (cd /mnt/c && "$EXE" relais --tube orchestrateur-test) >/dev/null; sleep 2; }

quitter 2>/dev/null
mkdir -p "$TEST"
rm -f "$TEST/etat.json" "$TEST/journal.log"
printf 'tube = "orchestrateur-test"\n' > "$TEST/config.toml"
cp "$DEPOT/target/x86_64-pc-windows-gnu/release/orchestrateur.exe" "$EXE"

etape "1. Relais sans orchestrateur, puis lancement automatique par le client"
(cd /mnt/c && echo '{"methode":"GET","chemin":"/etat"}' | "$EXE" relais --tube orchestrateur-test >/dev/null 2>&1); code=$?
verifie "relais : code 69 quand l'orchestrateur est absent" '[ $code = 69 ]'
s=$("$CHARGE" statut 2>&1)
echo "$s" | head -4
verifie "statut via relais après lancement automatique" 'echo "$s" | grep -q "Orchestrateur (époque 1)"'
verifie "une instance" '[ "$(instances)" = 1 ]'

etape "2. Coût d'un « charge statut » via le relais (5 appels)"
for i in 1 2 3 4 5; do /usr/bin/time -f "%e s" "$CHARGE" statut >/dev/null; done 2>&1

etape "3. demande, attend au premier plan, fin"
libre=$("$CHARGE" statut | awk '/RAM Windows +disponible/{for(i=1;i<=NF;i++) if($i=="libre"){print $(i+1)}}' | tr , .)
A=$(awk -v l="$libre" 'BEGIN{printf "%.1fG", l*0.7}')
B=$(awk -v l="$libre" 'BEGIN{printf "%.1fG", l*0.5}')
echo "RAM Windows libre : $libre Go → A $A, B $B"
t1=$("$CHARGE" demande --cote windows --ram "$A" "A (occupe la RAM Windows)" 2>/dev/null)
t2=$("$CHARGE" demande --cote windows --ram "$B" "B" 2>/dev/null)
"$CHARGE" attend "$t2" --max 3s; code=$?
verifie "attend --max : code 75, place gardée" '[ $code = 75 ] && "$CHARGE" statut | grep -qE "^#$t2 +attente"'
( sleep 2; "$CHARGE" fin "$t1" >/dev/null ) &
"$CHARGE" attend "$t2" --max 1m > "$TEST/attend.txt"; code=$?
cat "$TEST/attend.txt"
verifie "attend rend 0 au feu vert" '[ $code = 0 ] && grep -q "FEU VERT pour le ticket #$t2" "$TEST/attend.txt"'
"$CHARGE" prolonge "$t2" 15m "essai de prolongation"; code=$?
verifie "prolonge" '[ $code = 0 ]'
"$CHARGE" fin "$t2" >/dev/null

etape "4. run : code rendu, puis --max et rattachement"
"$CHARGE" run --cote windows --ram 1G "écho" -- sh -c 'echo bonjour; exit 5'; code=$?
verifie "run rend le code de la commande" '[ $code = 5 ]'
t3=$("$CHARGE" demande --cote windows --ram "$A" "occupant" 2>/dev/null)
"$CHARGE" run --cote windows --ram "$B" --max 3s "patient" -- echo enfin; code=$?
verifie "run --max : code 75" '[ $code = 75 ]'
( sleep 3; "$CHARGE" fin "$t3" >/dev/null ) &
sortie=$("$CHARGE" run --cote windows --ram "$B" --max 1m "patient" -- echo enfin 2>&1); code=$?
echo "$sortie"
verifie "la même commande reprend son ticket et part" '[ $code = 0 ] && echo "$sortie" | grep -q "repris" && echo "$sortie" | grep -q "^enfin$"'

etape "5. Arrêt propre par le tube"
wait
quitter
verifie "plus aucune instance" '[ "$(instances)" = 0 ]'
verifie "journal : ARRÊT" 'grep -q "ARRÊT demandé par le tube" "$TEST/journal.log"'

etape "Journal"
cat "$TEST/journal.log"
echo
echo "Échecs : $echecs"
exit $echecs
