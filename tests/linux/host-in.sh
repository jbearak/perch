#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
#
# The M4 host checks, run as root inside the container host.Dockerfile
# builds, with the binary under test at /usr/local/bin/perch. Every login is
# an ssh connection to perch-user@localhost through PAM, so each one is a
# logind session with its own scope. The ssh keys and agents are made here
# and thrown away with the container.
#
# Runs every check, prints one PASS or FAIL line per check and a summary,
# and exits non-zero if any failed. `host-in.sh <check>...` runs only those.
set -uo pipefail

U=perch-user
W=/root/m4
mkdir -p "$W"
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR
    -o BatchMode=yes -o IdentitiesOnly=yes -i "$W/key")

note() { echo "    $*"; }
die() {
    echo "    FAILED: $*"
    exit 1
}

# A command as perch-user over a fresh ssh login (no terminal).
on() { ssh "${SSH_OPTS[@]}" "$U@localhost" "$@"; }

# `bg_tty NAME CMD [ssh options...]`: CMD over ssh with a terminal, in the
# background, its output in $W/NAME.out, its pid in BG. Standard input stays
# open (and silent) for as long as ssh runs.
bg_tty() {
    local name=$1 cmd=$2
    shift 2
    : >"$W/$name.out"
    ssh -tt "${SSH_OPTS[@]}" "$@" "$U@localhost" "$cmd" \
        < <(exec sleep 600) >"$W/$name.out" 2>&1 &
    BG=$!
}

# `wait_for SECONDS CMD...`: polls CMD every 0.2 seconds until it succeeds.
wait_for() {
    local deadline=$((SECONDS + $1))
    shift
    until "$@" >/dev/null 2>&1; do
        [ $SECONDS -lt $deadline ] || return 1
        sleep 0.2
    done
}

gone() { ! kill -0 "$1" 2>/dev/null; }
field() { on "perch info $1 --json" | jq -r ".$2"; }
ticks() { on "cat ticks-$1"; }
at_least() { [ "$(ticks "$1")" -ge "$2" ]; }
has() { grep -qF -- "$2" "$W/$1.out"; }
attached_is() { [ "$(field "$1" attached)" = "$2" ]; }

# `attach_check SESSION [PERCH [WANT]]`: attaches over a new login, checks
# that the client receives WANT, by default a line the program printed before
# the attach (so it came from the restore, not live output), and then live
# output, then detaches.
attach_check() {
    local s=$1 bin=${2:-perch} t0 want
    wait_for 15 at_least "$s" 3 || die "$s: the program is not ticking"
    t0=$(ticks "$s")
    want=${3:-"[$s:$((t0 - 2))]"}
    bg_tty "attach-$s" "$bin attach $s"
    local pid=$BG
    wait_for 20 has "attach-$s" "$want" || {
        cat -v "$W/attach-$s.out" | head -c 3000
        die "$s: the restored screen has no $want"
    }
    wait_for 10 has "attach-$s" "[$s:$((t0 + 2))]" || die "$s: no live output after the restore"
    wait_for 10 attached_is "$s" true || die "$s: not attached"
    on "perch detach $s" || die "$s: perch detach failed"
    wait_for 10 gone "$pid" || die "$s: the attach client did not exit after detach"
    note "$s: attach with $bin restored \"$want\", then live output; detached"
}

# The ssh daemon processes of the one live terminal login.
sshd_of() {
    # The connection's user-side process prints its pid as $PPID; its
    # parent is the privileged monitor for the same connection.
    local user_side monitor
    user_side=$(sed -n 's/^SSHD=\([0-9]*\).*/\1/p' "$W/$1.out" | head -1)
    [ -n "$user_side" ] || return 1
    monitor=$(ps -o ppid= -p "$user_side" | tr -d ' ')
    case "$(ps -o args= -p "$monitor")" in
        *perch-user*) echo "$user_side $monitor" ;;
        *) echo "$user_side" ;;
    esac
}

logind_conf() {
    mkdir -p /etc/systemd/logind.conf.d
    printf '[Login]\nKillUserProcesses=%s\n' "$1" >/etc/systemd/logind.conf.d/perch-test.conf
    systemctl restart systemd-logind
    wait_for 10 sh -c "loginctl show-session -p KillUserProcesses | grep -qx KillUserProcesses=$1" ||
        die "logind did not take KillUserProcesses=$1"
}

# --- checks -----------------------------------------------------------------

check_ssh_drop() {
    bg_tty drop 'echo SSHD=$PPID; exec perch new --name drop -- perch-ticker drop'
    local client=$BG sup pids before
    wait_for 15 at_least drop 3 || die "the session did not start"
    sup=$(field drop supervisorPid)
    pids=$(sshd_of drop) || die "cannot find the login's sshd"
    note "killing sshd $pids (the ssh client is $client)"
    kill -9 $pids
    wait_for 10 gone "$client" || die "the ssh client is still running"
    wait_for 10 attached_is drop false || die "the session still has a client"
    before=$(ticks drop)
    wait_for 10 at_least drop $((before + 3)) || die "the program stopped after the drop"
    [ "$(field drop supervisorPid)" = "$sup" ] || die "the supervisor changed"
    note "the program ran on: tick $before, then $(ticks drop)"
    attach_check drop
    on "perch kill drop" || die "kill failed"
}

check_logind() {
    local scope sup sid
    # KillUserProcesses=no: the session outlives the login that made it.
    logind_conf no
    out=$(on 'echo "$XDG_SESSION_ID"; perch new -d --name lg-no -- perch-ticker lg-no >/dev/null &&
        perch doctor --json > doctor-no.json; perch info lg-no --json') || die "login failed"
    sid=$(head -1 <<<"$out")
    sup=$(tail -n +2 <<<"$out" | jq -r .supervisorPid)
    scope=$(cut -d: -f3 "/proc/$sup/cgroup")
    [ "$scope" = "/user.slice/user-$(id -u $U).slice/session-$sid.scope" ] ||
        die "the supervisor is in $scope, not login $sid's scope"
    note "KillUserProcesses=no: supervisor $sup is in $scope; the login has ended"
    on 'jq -e ".checks[] | select(.name == \"logind\") | .status == \"ok\"" doctor-no.json' >/dev/null ||
        die "doctor warned with KillUserProcesses=no"
    wait_for 10 sh -c "! loginctl show-session $sid -p State 2>/dev/null | grep -q active" ||
        die "login $sid did not end"
    sleep 2
    kill -0 "$sup" 2>/dev/null || die "the supervisor died with the login"
    attach_check lg-no
    on "perch kill lg-no" || die "kill failed"

    # KillUserProcesses=yes: logind ends the session with the login, and
    # doctor said it would.
    logind_conf yes
    out=$(on 'echo "$XDG_SESSION_ID"; perch new -d --name lg-yes -- perch-ticker lg-yes >/dev/null &&
        perch doctor --json > doctor-yes.json; perch info lg-yes --json') || die "login failed"
    sid=$(head -1 <<<"$out")
    sup=$(tail -n +2 <<<"$out" | jq -r .supervisorPid)
    on 'jq -c ".checks[] | select(.name == \"logind\")" doctor-yes.json' | sed 's/^/    doctor: /'
    on 'jq -e "(.checks[] | select(.name == \"logind\") | .status == \"warn\") and .survivesLogout == false" doctor-yes.json' \
        >/dev/null || die "doctor did not warn with KillUserProcesses=yes"
    wait_for 15 gone "$sup" || die "supervisor $sup outlived login $sid with KillUserProcesses=yes"
    state=$(on "perch list --all --json" | jq -r '.sessions[] | select(.name == "lg-yes") | .state')
    case "$state" in
        "" | lost | exited) note "KillUserProcesses=yes: supervisor $sup ended with login $sid (list: ${state:-absent})" ;;
        *) die "lg-yes is still $state" ;;
    esac
    on "perch dismiss lg-yes" >/dev/null 2>&1 || true
    logind_conf no
}

check_agent_forwarding() {
    local a b fp_a fp_b
    for n in a b; do
        ssh-keygen -q -t ed25519 -N "" -C "agent-$n" -f "$W/agent-key-$n"
    done
    a=$(ssh-agent -s -a "$W/agent-a.sock" | sed -n 's/^SSH_AGENT_PID=\([0-9]*\).*/\1/p')
    b=$(ssh-agent -s -a "$W/agent-b.sock" | sed -n 's/^SSH_AGENT_PID=\([0-9]*\).*/\1/p')
    SSH_AUTH_SOCK=$W/agent-a.sock ssh-add -q "$W/agent-key-a"
    SSH_AUTH_SOCK=$W/agent-b.sock ssh-add -q "$W/agent-key-b"
    fp_a=$(ssh-keygen -lf "$W/agent-key-a.pub" | cut -d' ' -f2)
    fp_b=$(ssh-keygen -lf "$W/agent-key-b.pub" | cut -d' ' -f2)

    # Through agent A: a shell whose SSH_AUTH_SOCK is the session's link.
    SSH_AUTH_SOCK=$W/agent-a.sock ssh -A "${SSH_OPTS[@]}" "$U@localhost" 'perch new -d --name agent --link-env SSH_AUTH_SOCK -- bash --norc --noprofile -i >/dev/null &&
        perch send agent --submit --text "{ echo \"\$SSH_AUTH_SOCK\"; ssh-add -l; } > agent.tmp 2>&1; mv agent.tmp agent-before" &&
        for i in $(seq 50); do [ -s agent-before ] && break; sleep 0.2; done; cat agent-before' >"$W/agent-before" ||
        die "creating the session through agent A failed"
    sed 's/^/    first login: /' "$W/agent-before"
    grep -q '/env/SSH_AUTH_SOCK$' "$W/agent-before" || die "SSH_AUTH_SOCK is not the session's link"
    grep -qF "$fp_a" "$W/agent-before" || die "agent A was not reachable through the link"

    # Agent A's forwarded socket is gone with that login. Attach through
    # agent B, then ask again from inside the session.
    SSH_AUTH_SOCK=$W/agent-b.sock bg_tty attach-agent "perch attach agent" -A
    local client=$BG
    wait_for 15 attached_is agent true || die "attach through agent B failed"
    on 'rm -f agent-after; perch send agent --submit --text "ssh-add -l > agent.tmp 2>&1; mv agent.tmp agent-after" &&
        for i in $(seq 50); do [ -s agent-after ] && break; sleep 0.2; done; cat agent-after' >"$W/agent-after"
    sed 's/^/    after reattach: /' "$W/agent-after"
    on "perch detach agent"
    wait_for 10 gone "$client" || true
    on "perch kill agent"
    kill "$a" "$b"
    grep -qF "$fp_b" "$W/agent-after" || die "ssh-add -l inside the session did not reach agent B"
    ! grep -qF "$fp_a" "$W/agent-after" || die "ssh-add -l inside the session still reached agent A"
    note "SSH_AUTH_SOCK through the link reached the attached client's agent (B)"
}

check_tmpfiles() {
    local uid dead_dir live_dir dead_sup conf=/run/perch-test-tmpfiles.conf
    uid=$(id -u $U)
    on "perch new -d --name tmp-live -- perch-ticker tmp-live >/dev/null &&
        perch new -d --name tmp-dead -- perch-ticker tmp-dead >/dev/null" || die "cannot create sessions"
    live_dir=$(field tmp-live dir)
    dead_dir=$(field tmp-dead dir)
    dead_sup=$(field tmp-dead supervisorPid)
    kill -9 "$dead_sup"
    wait_for 10 gone "$dead_sup" || die "cannot kill the supervisor"
    # Age 0: everything under the runtime directory is old enough to go.
    echo "e /tmp/perch-$uid - - - 0" >"$conf"
    sleep 1
    systemd-tmpfiles --clean "$conf" || die "systemd-tmpfiles --clean failed"
    rm -f "$conf"
    [ ! -e "$dead_dir" ] || die "the dead session's directory $dead_dir is still there"
    for f in "" /sock /meta.json /supervisor.log; do
        [ -e "$live_dir$f" ] || die "the live session lost $live_dir$f"
    done
    [ "$(field tmp-live state)" = running ] || die "the live session is not running"
    note "tmpfiles removed $dead_dir and kept $live_dir"
    attach_check tmp-live
    on "perch kill tmp-live"
}

check_binary_replaced() {
    local dir sup log gen1 gen2 exe
    dir=$(on 'mktemp -d') || die "mktemp"
    on "cp /usr/local/bin/perch $dir/perch && $dir/perch new -d --name bin -- perch-ticker bin >/dev/null &&
        rm -rf $dir && mkdir $dir && cp /usr/local/bin/perch $dir/perch.new && mv $dir/perch.new $dir/perch" ||
        die "cannot start from $dir"
    sup=$(field bin supervisorPid)
    case "$(readlink "/proc/$sup/exe")" in
        *"(deleted)") note "supervisor $sup runs $(readlink "/proc/$sup/exe")" ;;
        *) die "the supervisor's binary was not deleted: $(readlink "/proc/$sup/exe")" ;;
    esac
    attach_check bin /usr/local/bin/perch
    # The replacement goes too: from here on nothing exists at the path the
    # supervisor started from, so a model restart that exec'd it would fail.
    on "rm -rf $dir" || die "cannot remove $dir"
    absent() { [ ! -e "$dir/perch" ] || die "$dir/perch exists $1"; }
    absent "before the model is killed"
    log="$(field bin dir)/supervisor.log"
    gen1=$(sed -n 's/.*model process \([0-9]*\) started (generation 1.*/\1/p' "$log" | tail -1)
    [ -n "$gen1" ] || die "no model process in $log"
    kill -9 "$gen1"
    wait_for 15 grep -q 'started (generation 2' "$log" || {
        tail -5 "$log"
        die "the model did not restart"
    }
    gen2=$(sed -n 's/.*model process \([0-9]*\) started (generation 2.*/\1/p' "$log" | tail -1)
    kill -0 "$gen2" || die "the restarted model $gen2 is not running"
    absent "after the model restarted"
    # Forked from the supervisor, not exec'd from any file on disk.
    exe=$(readlink "/proc/$gen2/exe")
    case "$exe" in
        "$dir/perch (deleted)") note "killed model $gen1; model $gen2 runs $exe" ;;
        *) die "the restarted model $gen2 runs $exe, not the supervisor's deleted binary" ;;
    esac
    # A restarted model has not seen the screen: the restore says so and
    # asks the program to redraw (the design's partial restore).
    [ "$(field bin model)" = partial ] || die "the model is $(field bin model), not partial"
    attach_check bin /usr/local/bin/perch "The screen could not be restored. Asking the program to redraw."
    absent "after the attach"
    on "perch kill bin"
}

# --- main -------------------------------------------------------------------

ssh-keygen -q -t ed25519 -N "" -f "$W/key"
install -m 0600 -o $U -g $U "$W/key.pub" /home/$U/.ssh/authorized_keys
cat >/usr/local/bin/perch-ticker <<'EOF'
#!/bin/sh
# Prints `[NAME:N]` once a second, and N to ~/ticks-NAME.
i=0
while :; do
    i=$((i + 1))
    echo "[$1:$i]"
    echo "$i" >"$HOME/ticks-$1"
    sleep 1
done
EOF
chmod 755 /usr/local/bin/perch-ticker

on 'perch version' | sed 's/^/perch: /'
checks=("$@")
[ ${#checks[@]} -gt 0 ] || checks=(ssh_drop logind agent_forwarding tmpfiles binary_replaced)
failed=0
for c in "${checks[@]}"; do
    echo "== $c"
    if (check_$c); then
        echo "PASS $c"
    else
        echo "FAIL $c"
        failed=$((failed + 1))
    fi
done
echo "== host checks: $((${#checks[@]} - failed)) of ${#checks[@]} passed"
[ $failed -eq 0 ]
