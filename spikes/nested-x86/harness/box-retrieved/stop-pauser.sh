#!/bin/bash
# stop the n3 pauser loop (safe /proc scan, excludes self and ssh)
ME=$$
for d in /proc/[0-9]*; do
  p=${d#/proc/}
  [ "$p" = "$ME" ] && continue
  c=$(tr "\0" " " < "$d/cmdline" 2>/dev/null) || continue
  case "$c" in
    *run-n3-pause.sh*) [ "$p" != "$PPID" ] && kill "$p" 2>/dev/null && echo "killed pauser-script $p" ;;
  esac
done
