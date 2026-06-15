#!/usr/bin/env bash
# Claude Code hook: record this session's working directory so the
# voice-dictation daemon can resolve file references for the *active* Claude
# session. gnome-terminal shares one process across all tabs, so the daemon
# cannot tell which session you are focused on from X11 alone — the most
# recently active session (the last to fire this hook) wins the tie.
#
# Wire it to UserPromptSubmit and SessionStart in ~/.claude/settings.json.
# Reads the hook JSON on stdin; always exits 0 so it never blocks Claude.
set -u
dir="${XDG_RUNTIME_DIR:-/tmp}/sunoto"
mkdir -p "$dir" 2>/dev/null || exit 0
cwd="$(python3 -c 'import sys, json; print(json.load(sys.stdin).get("cwd", ""))' 2>/dev/null)"
[ -n "$cwd" ] || exit 0
tmp="$(mktemp "$dir/.cwd.XXXXXX" 2>/dev/null)" || exit 0
printf '%s\n' "$cwd" >"$tmp" && mv -f "$tmp" "$dir/claude-active-cwd"
exit 0
