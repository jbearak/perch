#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
#
# Records the program fixtures under tests/fidelity/fixtures/ by running
# real programs under `script` at 80x24 with typed input, on a maintainer
# machine. CI never runs this: it replays the committed recordings.
#
#   tests/fidelity/record.sh [NAME...]     # default: every program found
#
# Each recording is cut just before the program's own exit (the last
# `ESC [?1049l`, or the typed `exit`/`q()`): the harness restores there,
# then feeds the exit and the output after it to both terminals.
#
# Programs: R, zsh (with a right prompt), vim (in and out), less. fish and
# a reedline program are recorded when installed (`fish`, and
# `$PERCH_REEDLINE` pointing at a reedline-based binary); otherwise
# gen/synthetic.mjs covers them with synthetic fixtures.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
out="$here/fixtures"
work=$(mktemp -d "${TMPDIR:-/tmp}/perch-record.XXXXXX")
trap 'rm -rf "$work"' EXIT

# A clean, fixed environment: no user rc files, no host or user names in
# prompts.
run_env() {
    env -i HOME="$work" TERM=xterm-256color LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 \
        PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin "$@"
}

# record NAME CUT_MARKER COMMAND < typed-input-script
# The input script is sh that prints keystrokes with pauses.
record() {
    name=$1 cut=$2 cmd=$3
    dir="$out/$name"
    mkdir -p "$dir"
    sh "$work/input-$name.sh" |
        run_env script -q "$dir/stream.bin" /bin/sh -c "stty rows 24 cols 80; cd \"\$HOME\"; $cmd" >/dev/null
    node "$here/gen/recorded-script.mjs" "$dir" "$cut" "$name"
    echo "recorded $name ($(wc -c <"$dir/stream.bin") bytes)"
}

SELECTED="$*"
# Whether NAME was asked for (everything when no names were given).
want_name() {
    [ -z "$SELECTED" ] && return 0
    for n in $SELECTED; do [ "$n" = "$1" ] && return 0; done
    return 1
}

seq 1 60 | sed 's/^/line /' >"$work/file.txt"
i=0
while [ $i -lt 120 ]; do
    printf '\033[3%dmcolored %d\033[0m plain text after it\n' $((i % 8)) $i
    i=$((i + 1))
done >"$work/colors.txt"

if want_name r-prompt && command -v R >/dev/null; then
    cat >"$work/input-r-prompt.sh" <<'EOF'
p() { sleep 0.6; printf '%s\r' "$1"; }
sleep 2
p 'set.seed(1); x <- round(rnorm(8), 2)'
p 'print(x)'
p 'summary(cars)'
p 'cat("\033[1;32mok\033[0m and \033[4mlink-free\033[0m text\n")'
p 'pb <- txtProgressBar(max = 20, style = 3); for (i in 1:20) { setTxtProgressBar(pb, i); Sys.sleep(0.02) }; close(pb)'
p 'head(mtcars, 3)'
sleep 1
p 'q("no")'
sleep 1
EOF
    record r-prompt 'q("no")' 'R --vanilla -q; echo after R'
fi

if want_name zsh-right-prompt && command -v zsh >/dev/null; then
    cat >"$work/input-zsh-right-prompt.sh" <<'EOF'
p() { sleep 0.4; printf '%s\r' "$1"; }
sleep 1
p 'echo hello'
p "print -P '%F{red}red%f %Bbold%b %K{blue}bg%k'"
p 'ls -1 /usr | head -5'
p 'false'
p 'for i in 1 2 3; do echo "item $i"; done'
sleep 0.5
p 'exit'
sleep 0.5
EOF
    mkdir -p "$work/zdot" "$work/project"
    printf '%s\n' "PROMPT='%1~ %# '" "RPROMPT='[%?] right'" >"$work/zdot/.zshrc"
    record zsh-right-prompt exit "cd project; ZDOTDIR=\"\$HOME/zdot\" exec zsh -i"
fi

if want_name fish-right-prompt-recorded && command -v fish >/dev/null; then
    cat >"$work/input-fish-right-prompt-recorded.sh" <<'EOF'
p() { sleep 0.6; printf '%s\r' "$1"; }
sleep 2
p 'function fish_right_prompt; echo "[$status] right"; end'
p 'echo hello'
p 'false'
sleep 0.5
p 'exit'
sleep 0.5
EOF
    record fish-right-prompt-recorded exit 'exec fish --no-config'
fi

if want_name reedline-recorded && [ -n "${PERCH_REEDLINE:-}" ]; then
    cat >"$work/input-reedline-recorded.sh" <<'EOF'
p() { sleep 0.6; printf '%s\r' "$1"; }
sleep 1
p 'one'
p 'two'
sleep 0.5
printf '\004'
sleep 0.5
EOF
    record reedline-recorded '' "exec $PERCH_REEDLINE"
fi

if want_name vim-in-out && command -v vim >/dev/null; then
    cat >"$work/input-vim-in-out.sh" <<'EOF'
k() { sleep 0.4; printf '%s' "$1"; }
sleep 1
k ':set number hlsearch'; k "$(printf '\r')"
k '/line 1'; k "$(printf '\r')"
k 'G'
k 'ggOinserted at the top'; k "$(printf '\033')"
k ':q!'; k "$(printf '\r')"
sleep 0.5
EOF
    record vim-in-out '\033[?1049l' 'vim -u NONE -N -i NONE file.txt; echo after vim; echo more output'
fi

if want_name less && command -v less >/dev/null; then
    cat >"$work/input-less.sh" <<'EOF'
k() { sleep 0.4; printf '%s' "$1"; }
sleep 1
k ' '
k '/colored 5'; k "$(printf '\r')"
k 'q'
sleep 0.5
EOF
    record less '\033[?1049l' 'less -R colors.txt; echo after less'
fi
