#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
#
# The M4 host checks: SSH drop, logind, agent forwarding, tmpfiles and a
# deleted binary, in a disposable privileged container with systemd as
# PID 1 (tests/linux/host.Dockerfile), run by tests/linux/host-in.sh.
#
#   tests/linux/host-checks.sh [PERCH_BINARY] [CHECK...]
#
# The binary must be a Linux one for Docker's architecture; it defaults to
# target/linux-hosts/perch, built by tests/linux/build-musl.sh if missing.
# The container is removed afterwards.
#
# If systemd or logind cannot run in the container this fails. Under
# colima, PERCH_COLIMA_PROFILE=1 runs the checks in a separate colima
# profile made for the run and deleted after it instead; the default
# profile is never changed. That is a manual fallback for VMs that cannot
# run systemd containers, and CI never uses it.
set -eu

cd "$(git rev-parse --show-toplevel)"
bin=${1:-target/linux-hosts/perch}
[ $# -eq 0 ] || shift
if [ "${PERCH_COLIMA_PROFILE:-}" = 1 ]; then
    # Everything, the build included, runs against the temporary daemon.
    # --activate=false leaves the user's Docker context alone; DOCKER_HOST
    # selects the temporary daemon.
    profile=perch-m4-$$
    trap 'colima delete --force "$profile" >/dev/null 2>&1 || true' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    colima start --profile "$profile" --activate=false --cpu 2 --memory 2 --disk 20 --runtime docker
    DOCKER_HOST="unix://$HOME/.colima/$profile/docker.sock"
    export DOCKER_HOST
    PERCH_COLIMA_PROFILE= "$0" "$bin" "$@"
    exit
fi

if [ ! -x "$bin" ]; then
    [ "$bin" = target/linux-hosts/perch ] || { echo "no binary at $bin" >&2; exit 2; }
    tests/linux/build-musl.sh "$bin"
fi

name=perch-host-$$
trap 'docker rm -f "$name" >/dev/null 2>&1 || true' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
docker build -q -f tests/linux/host.Dockerfile -t perch-host tests/linux >/dev/null
docker run -d --name "$name" --privileged --cgroupns=private \
    --tmpfs /run --tmpfs /run/lock perch-host >/dev/null

# systemd reports `degraded` if any unit failed; logind and sshd are what
# matter.
state=$(docker exec "$name" timeout 60 systemctl is-system-running --wait || true)
echo "systemd: $state"
for unit in systemd-logind.service ssh.service dbus.service; do
    if ! docker exec "$name" systemctl is-active --quiet "$unit"; then
        echo "FAIL: $unit is not running in the container; systemd/logind cannot run here" >&2
        docker exec "$name" systemctl --failed --no-pager >&2 || true
        docker exec "$name" journalctl -b --no-pager -n 50 >&2 || true
        exit 1
    fi
done

docker cp "$bin" "$name:/usr/local/bin/perch"
docker cp tests/linux/host-in.sh "$name:/root/host-in.sh"
docker exec "$name" chmod 755 /usr/local/bin/perch
docker exec "$name" bash /root/host-in.sh "$@"
