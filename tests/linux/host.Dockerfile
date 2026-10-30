# SPDX-License-Identifier: GPL-3.0-only
#
# A disposable Linux host for the M4 checks: systemd as PID 1 with logind,
# openssh-server through PAM (so each ssh login gets a logind session and
# its scope), and a test user. The ssh client, its keys and its agents live
# in the same container; nothing on the machine that runs it is used.
# tests/linux/host-checks.sh builds and runs it.

FROM ubuntu:24.04

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        systemd systemd-sysv dbus libpam-systemd openssh-server openssh-client \
        jq procps psmisc \
    && rm -rf /var/lib/apt/lists/*

# Only what the checks need: no getty on a console nobody watches.
RUN systemctl mask getty.target console-getty.service systemd-firstboot.service \
        systemd-resolved.service systemd-networkd.service \
    && systemctl enable ssh.service \
    && useradd -m -s /bin/bash perch-user \
    && install -d -m 0700 -o perch-user -g perch-user /home/perch-user/.ssh \
    && printf '%s\n' 'UsePAM yes' 'PasswordAuthentication no' 'AllowAgentForwarding yes' \
        'ClientAliveInterval 0' > /etc/ssh/sshd_config.d/perch-test.conf

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
