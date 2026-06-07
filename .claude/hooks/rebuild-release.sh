#!/usr/bin/env bash
# Background release rebuild of the Zord GUI, fired by the Stop hook so the
# latest code is compiled after each turn. Detached (nohup + &) so it returns
# immediately and never blocks/freezes the session — cargo's incremental build
# means only the changed crates recompile. Progress + errors land in the log;
# cargo's own target-dir lock serializes overlapping runs safely.
#
# Wired from .claude/settings.local.json:
#   "hooks": { "Stop": [ { "hooks": [ { "type": "command",
#     "command": "bash /Users/jacobhaig/Documents/GitHub/zord/.claude/hooks/rebuild-release.sh" } ] } ] }
LOG=/tmp/zord-release-build.log
cd /Users/jacobhaig/Documents/GitHub/zord || exit 0
nohup cargo build --release -p zord-gui \
  --features parakeet,diarization,llm-remote,llm-local \
  > "$LOG" 2>&1 &
exit 0
