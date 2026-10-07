#!/bin/sh
# STAND-IN for the media supervisor's tests (crates/qworkerd/tests/media.rs and the server's
# crates/server/tests/outputs_video.rs). It is not ffmpeg and proves nothing about ffmpeg: it exists so
# the supervision around a real process (groups, signals, state files, attach, restart) can be tested
# without the binary. The real renders are crates/server/tests/outputs_video_ffmpeg.rs, which needs
# QUANSIO_TEST_FFMPEG_BIN and is BLOCKED_EXTERNAL without it.
#
# Test-only arguments the plan builder never produces:
#   --stand-in-stall        hang (a child that shares the process group; its pid is logged as `child <pid>`)
#   --stand-in-slow         take a second
#   --stand-in-fail         exit 3 with a message on stderr
#   --stand-in-log=<file>   append "<pid> <cwd>" to <file>
case "$1" in -version) echo "ffmpeg version stand-in (not ffmpeg)"; exit 0;; esac
log=$(echo "$*" | sed -n 's/.*--stand-in-log=\([^ ]*\).*/\1/p')
[ -n "$log" ] && echo "$$ $PWD" >> "$log"
for last; do :; done
case "$*" in
  *--stand-in-stall*) sleep 60 & [ -n "$log" ] && echo "child $!" >> "$log"; wait;;
  *--stand-in-slow*) sleep 1;;
  *--stand-in-fail*) echo "boom: stand-in failure" >&2; exit 3;;
esac
case "$*" in
  *signalstats*)
    file=$(echo "$*" | sed -n 's/.*metadata=print:file=\([^ ]*\).*/\1/p')
    case "$file" in
      stats_01.txt|stats_03.txt) printf 'lavfi.signalstats.YMIN=16\nlavfi.signalstats.YMAX=16\nlavfi.signalstats.UMIN=128\nlavfi.signalstats.UMAX=128\nlavfi.signalstats.VMIN=128\nlavfi.signalstats.VMAX=128\n' > "$file";;
      *) printf 'lavfi.signalstats.YMIN=16\nlavfi.signalstats.YMAX=235\nlavfi.signalstats.UMIN=100\nlavfi.signalstats.UMAX=140\nlavfi.signalstats.VMIN=100\nlavfi.signalstats.VMAX=140\n' > "$file";;
    esac
    exit 0;;
esac
case "$last" in
  sheet.png) printf '\211PNG\r\n\032\n\000\000\000\rIHDR\000\000\003\350\000\000\001\220\010\002\000\000\000sheet %s %s' "$*" "$(ls frame_*.png 2>/dev/null | tr '\n' ' ')" > "$last";;
  *.png) printf '\211PNG\r\n\032\n\000\000\000\rIHDR\000\000\001\100\000\000\000\264\010\002\000\000\000%s' "$last" > "$last";;
  *) printf 'xxxxftypisom stand-in output %s' "$*" > "$last";;
esac
exit 0
