#!/bin/ksh
# Copyright (c) 2025 Robert August Vincent II <pillarsdotnet@gmail.com>
# Co-author: Cursor-AI.
# Combined timesheet CLI: start, stop, list, started, timeoff, workalias, install, rotate.

TIMESHEET=~/Documents/timesheet.log
TS_DIR=$(dirname "$0")

# Rotate log: move/append to timesheet.YYMMDD using most recent entry's timestamp.
# If the destination already exists (same day), append lines instead of replacing.
do_rotate() {
  if [ ! -f "$TIMESHEET" ]; then
    echo "ts rotate: no timesheet data found." >&2
    return 1
  fi
  max_epoch=$(awk -F'|' 'BEGIN{m=0} /^START\|/ || /^STOP\|/ {if($2>m)m=$2} END{print m+0}' "$TIMESHEET")
  if [ -z "$max_epoch" ] || [ "$max_epoch" -eq 0 ]; then
    echo "ts rotate: no valid entries in timesheet." >&2
    return 1
  fi
  stamp=$(date -r "$max_epoch" +%y%m%d 2>/dev/null) || stamp=$(date -d "@$max_epoch" +%y%m%d 2>/dev/null)
  if [ -z "$stamp" ]; then
    echo "ts rotate: could not format timestamp." >&2
    return 1
  fi
  dest="${TIMESHEET%.log}.$stamp"
  if [ -f "$dest" ]; then
    cat "$TIMESHEET" >> "$dest"
    rm -f "$TIMESHEET"
    echo "Appended to $dest"
  else
    mv "$TIMESHEET" "$dest"
    echo "Rotated $TIMESHEET to $dest"
  fi
}

# If the last log entry is from the previous week (before this week's Sunday 00:00:00), rotate first.
maybe_rotate_if_previous_week() {
  [ ! -f "$TIMESHEET" ] || [ ! -s "$TIMESHEET" ] && return
  last_line=$(tail -n 1 "$TIMESHEET" 2>/dev/null)
  case "$last_line" in
    'START|'*) last_epoch=$(echo "$last_line" | cut -d'|' -f2) ;;
    'STOP|'*)  last_epoch=$(echo "$last_line" | cut -d'|' -f2) ;;
    *) return ;;
  esac
  today_ymd=$(date +%Y-%m-%d)
  today_start=$(date -d "${today_ymd} 00:00:00" +%s 2>/dev/null) || today_start=$(date -j -f "%Y-%m-%d %H:%M:%S" "${today_ymd} 00:00:00" +%s 2>/dev/null)
  dow=$(date +%w)
  week_start=$((today_start - dow * 86400))
  [ -n "$last_epoch" ] && [ "$last_epoch" -lt "$week_start" ] && do_rotate
}

# Resolve optional list argument to a single timesheet file path. Sets LIST_INPUT.
# Arg can be: "log" (current), exact path, or extension match (e.g. 260220, 20260220, 0220, 2/20).
resolve_list_input() {
  list_arg=$1
  LIST_INPUT=""
  if [ -z "$list_arg" ]; then
    LIST_INPUT=$TIMESHEET
    return
  fi
  # Exact path (current or rotated)
  if [ -f "$list_arg" ]; then
    LIST_INPUT=$list_arg
    return
  fi
  # "log" → current timesheet
  if [ "$list_arg" = "log" ]; then
    LIST_INPUT=$TIMESHEET
    return
  fi
  # Resolve timesheet directory and find candidates (timesheet.log, timesheet.YYMMDD, ...)
  ts_dir=$(cd $(dirname "$TIMESHEET") 2>/dev/null && pwd)
  [ -z "$ts_dir" ] && return
  base="$ts_dir/timesheet"
  match_count=0
  match_file=""
  # Normalize arg to YYMMDD for comparison: 20260220 → 260220; 2/20 or 02/20 → 260220
  norm=""
  case "$list_arg" in
    [0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]) norm=${list_arg#??}; ;;  # last 6 digits
    */*) m=${list_arg%/*}; d=${list_arg#*/}; norm=$(printf "%s%02d%02d" $(date +%y) "$m" "$d" 2>/dev/null); ;;
  esac
  for f in "$base.log" "$base".[0-9]*; do
    [ ! -f "$f" ] && continue
    suffix=${f##*.}
    [ -z "$suffix" ] && continue
    # Match: exact, arg is substring of suffix, suffix is substring of arg, or normalized eq
    matched=false
    if [ "$list_arg" = "$suffix" ]; then matched=true; fi
    if [ "$matched" = false ] && case "$suffix" in *"$list_arg"*) true;; *) false;; esac; then matched=true; fi
    if [ "$matched" = false ] && case "$list_arg" in *"$suffix"*) true;; *) false;; esac; then matched=true; fi
    if [ "$matched" = false ] && [ -n "$norm" ] && [ "$norm" = "$suffix" ]; then matched=true; fi
    if [ "$matched" = true ]; then
      match_count=$((match_count + 1))
      match_file=$f
    fi
  done
  if [ "$match_count" -eq 1 ]; then
    LIST_INPUT=$match_file
  elif [ "$match_count" -gt 1 ]; then
    echo "ts list: multiple timesheets match \"$list_arg\"." >&2
    LIST_INPUT=""
  else
    echo "ts list: no timesheet matches \"$list_arg\"." >&2
    LIST_INPUT=""
  fi
}

if [ $# -lt 1 ]; then
  echo "Usage: ts <command> [args...]" >&2
  echo "  start | stop | list | started | timeoff | workalias | install | rotate" >&2
  exit 1
fi

cmd=$1
shift

case "$cmd" in
  start)
    maybe_rotate_if_previous_week
    activity="${*:-misc/unspecified}"
    echo "START|$(date +%s)|${activity}" >> "$TIMESHEET"
    echo "Started: ${activity} at $(date)"
    ;;
  stop)
    maybe_rotate_if_previous_week
    now=$(date +%s)
    last=$(tail -n 1 "$TIMESHEET" 2>/dev/null)
    today=$(date +%Y-%m-%d)
    case "$last" in
      'STOP|'*)
        prev_epoch=$(echo "$last" | cut -d'|' -f2)
        prev_date=$(date -r "$prev_epoch" +%Y-%m-%d 2>/dev/null) || prev_date=$(date -d "@$prev_epoch" +%Y-%m-%d 2>/dev/null)
        if [ -n "$prev_date" ] && [ "$prev_date" = "$today" ]; then
          insert_start=$((prev_epoch + 1))
          temp=$(mktemp)
          trap 'rm -f "$temp"' EXIT
          sed '$d' "$TIMESHEET" > "$temp" && \
            echo "START|${insert_start}|misc/unspecified" >> "$temp" && \
            echo "STOP|$now" >> "$temp" && \
            mv "$temp" "$TIMESHEET"
        else
          echo "STOP|$now" >> "$TIMESHEET"
        fi
        ;;
      *)
        echo "STOP|$now" >> "$TIMESHEET"
        ;;
    esac
    echo "Stopped at $(date)"
    ;;
  list)
    list_arg="${1:-}"
    resolve_list_input "$list_arg"
    if [ -z "$LIST_INPUT" ]; then
      if [ -n "$list_arg" ]; then
        exit 1
      fi
      LIST_INPUT=$TIMESHEET
    fi
    TMP_ACT=/tmp/timesheet_act_$$
    TMP_LOG=/tmp/timesheet_log_$$
    if [ ! -f "$LIST_INPUT" ]; then
      echo "No timesheet data found."
      exit 0
    fi
    last=$(tail -n 1 "$LIST_INPUT" 2>/dev/null)
    work_in_progress=false
    # Only treat as work in progress when listing current log (no arg or arg is "log")
    if [ -z "$list_arg" ] || [ "$list_arg" = "log" ]; then
      case "$last" in
        'START|'*)
          work_in_progress=true
          { cat "$LIST_INPUT"; echo "STOP|$(date +%s)"; } > "$TMP_LOG"
          ;;
        *)
          cat "$LIST_INPUT" > "$TMP_LOG"
          ;;
      esac
    else
      cat "$LIST_INPUT" > "$TMP_LOG"
    fi
    awk -F'|' '
      function push(e, a) { stack_epoch[++n] = e; stack_act[n] = a }
      function pop(e) {
        if (n < 1) return
        start_epoch = stack_epoch[n]; start_act = stack_act[n]; n--
        dur = e - start_epoch
        if (dur <= 0) return
        act[start_act] += dur
        days = int(start_epoch / 86400); dow = (days + 4) % 7
        dow_sec[dow] += dur
      }
      /^START\|/ {
        if (n >= 1) pop($2)
        push($2, $3)
        next
      }
      /^STOP\|/  { pop($2); next }
      END {
        total = 0; for (a in act) total += act[a]
        if (total == 0) { print "No work recorded."; exit 0 }
        for (a in act) printf "%.1f\t%s\n", 100*act[a]/total, a
      }
    ' "$TMP_LOG" | sort -rn > "$TMP_ACT"
    awk -F'|' '
      function push(e, a) { stack_epoch[++n] = e; stack_act[n] = a }
      function pop(e) {
        if (n < 1) return
        start_epoch = stack_epoch[n]; start_act = stack_act[n]; n--
        dur = e - start_epoch
        if (dur <= 0) return
        act[start_act] += dur
        days = int(start_epoch / 86400); dow = (days + 4) % 7
        dow_sec[dow] += dur
      }
      /^START\|/ {
        if (n >= 1) pop($2)
        push($2, $3)
        next
      }
      /^STOP\|/  { pop($2); next }
      END {
        total = 0; for (a in act) total += act[a]
        if (total == 0) exit 0
        names[0]="Sunday"; names[1]="Monday"; names[2]="Tuesday"; names[3]="Wednesday"
        names[4]="Thursday"; names[5]="Friday"; names[6]="Saturday"
        for (d = 0; d <= 6; d++) printf "%s\t%.2f\n", names[d], dow_sec[d]/3600
      }
    ' "$TMP_LOG" > "${TMP_ACT}.dow"
    if [ -s "$TMP_ACT" ]; then
      awk -F'\t' '{ printf "%.1f%%  %s\n", $1, $2 }' "$TMP_ACT"
    fi
    if [ -s "${TMP_ACT}.dow" ]; then
      awk -F'\t' '{ printf "%s  %.2f\n", $1, $2 }' "${TMP_ACT}.dow"
    fi
    if [ "$work_in_progress" = true ]; then
      start_epoch=$(echo "$last" | cut -d'|' -f2)
      activity=$(echo "$last" | cut -d'|' -f3-)
      start_time=$(date -r "$start_epoch" 2>/dev/null) || start_time=$(date -d "@$start_epoch" 2>/dev/null) || start_time="?"
      now=$(date +%s)
      duration_sec=$((now - start_epoch))
      duration_min=$((duration_sec / 60))
      duration_hr=$((duration_min / 60))
      duration_rem=$((duration_min % 60))
      if [ "$duration_hr" -gt 0 ]; then
        duration_fmt="${duration_hr}h ${duration_rem}m"
      else
        duration_fmt="${duration_min}m"
      fi
      echo ""
      echo "Current Task: $activity, started $start_time, worked $duration_fmt"
    fi
    rm -f "$TMP_ACT" "${TMP_ACT}.dow" "$TMP_LOG"
    ;;
  started)
    if [ $# -lt 1 ]; then
      echo "Usage: ts started <start_time> [activity...]" >&2
      echo "  start_time is required (e.g. \"2025-02-16 09:00\" or \"9:00 AM\")." >&2
      exit 1
    fi
    start_time=$1
    shift
    activity="${*:-misc/unspecified}"
    epoch=$(date -d "$start_time" +%s 2>/dev/null)
    if [ -z "$epoch" ]; then
      epoch=$(date -j -f "%Y-%m-%d %H:%M:%S" "$start_time" +%s 2>/dev/null)
    fi
    if [ -z "$epoch" ]; then
      epoch=$(date -j -f "%Y-%m-%d %H:%M" "$start_time" +%s 2>/dev/null)
    fi
    if [ -z "$epoch" ]; then
      epoch=$(date -j -f "%H:%M" "$start_time" +%s 2>/dev/null)
    fi
    if [ -z "$epoch" ]; then
      echo "ts started: could not parse start time: $start_time" >&2
      exit 1
    fi
    maybe_rotate_if_previous_week
    today=$(date +%Y-%m-%d)
    last=$(tail -n 1 "$TIMESHEET" 2>/dev/null)
    case "$last" in
      'START|'*)
        start_epoch=$(echo "$last" | cut -d'|' -f2)
        start_date=$(date -r "$start_epoch" +%Y-%m-%d 2>/dev/null) || start_date=$(date -d "@$start_epoch" +%Y-%m-%d 2>/dev/null)
        if [ -n "$start_date" ] && [ "$start_date" = "$today" ]; then
          temp=$(mktemp)
          trap 'rm -f "$temp"' EXIT
          sed '$d' "$TIMESHEET" > "$temp" && \
            echo "START|${epoch}|${activity}" >> "$temp" && \
            mv "$temp" "$TIMESHEET"
          echo "Started: ${activity} at $(date -r "$epoch" 2>/dev/null || date -d "@$epoch" 2>/dev/null || echo "$epoch")"
          exit 0
        fi
        ;;
      'STOP|'*)
        stop_epoch=$(echo "$last" | cut -d'|' -f2)
        stop_date=$(date -r "$stop_epoch" +%Y-%m-%d 2>/dev/null) || stop_date=$(date -d "@$stop_epoch" +%Y-%m-%d 2>/dev/null)
        if [ "$epoch" -lt "$stop_epoch" ] && [ -n "$stop_date" ] && [ "$stop_date" = "$today" ]; then
          temp=$(mktemp)
          trap 'rm -f "$temp"' EXIT
          sed '$d' "$TIMESHEET" > "$temp" && \
            echo "START|${epoch}|${activity}" >> "$temp" && \
            echo "$last" >> "$temp" && \
            mv "$temp" "$TIMESHEET"
          echo "Started: ${activity} at $(date -r "$epoch" 2>/dev/null || date -d "@$epoch" 2>/dev/null || echo "$epoch")"
          exit 0
        fi
        ;;
    esac
    echo "START|${epoch}|${activity}" >> "$TIMESHEET"
    echo "Started: ${activity} at $(date -r "$epoch" 2>/dev/null || date -d "@$epoch" 2>/dev/null || echo "$epoch")"
    ;;
  timeoff)
    maybe_rotate_if_previous_week
    if [ -f "$TIMESHEET" ]; then
      last_line=$(tail -n 1 "$TIMESHEET" 2>/dev/null)
      case "$last_line" in
        'STOP|'*) echo "START|$(date +%s)|misc/unspecified" >> "$TIMESHEET" ;;
      esac
    fi
    if [ ! -f "$TIMESHEET" ]; then
      echo "No timesheet data."
      exit 0
    fi
    TMP_LOG=/tmp/timeoff_log_$$
    last_line=$(tail -n 1 "$TIMESHEET" 2>/dev/null)
    case "$last_line" in
      'START|'*)
        { cat "$TIMESHEET"; echo "STOP|$(date +%s)"; } > "$TMP_LOG"
        ;;
      *)
        cat "$TIMESHEET" > "$TMP_LOG"
        ;;
    esac
    awk -F'|' '
      function push(e, a) { stack_epoch[++n] = e; stack_act[n] = a }
      function pop(e) {
        if (n < 1) return
        start_epoch = stack_epoch[n]; n--
        dur = e - start_epoch
        if (dur <= 0) return
        total_sec += dur
        days = int(start_epoch / 86400)
        day_seen[days] = 1
      }
      /^START\|/ {
        if (n >= 1) pop($2)
        push($2, $3)
        next
      }
      /^STOP\|/  { pop($2); next }
      END {
        for (d in day_seen) num_days++
        total_hr = total_sec / 3600
        need_hr = 8 * num_days - total_hr
        printf "%d %.4f %.4f\n", num_days, total_hr, need_hr
      }
    ' "$TMP_LOG" | read num_days total_hr need_hr
    rm -f "$TMP_LOG"
    if [ -z "$num_days" ] || [ "$num_days" -eq 0 ]; then
      echo "No work recorded."
      exit 0
    fi
    if awk "BEGIN { exit ($need_hr > 0) ? 0 : 1 }" 2>/dev/null; then
      :
    else
      echo "Average already at least 8 hours per day worked. You may stop now."
      date
      exit 0
    fi
    now=$(date +%s)
    stop_epoch=$(awk "BEGIN { printf \"%.0f\", $now + $need_hr * 3600 }")
    echo "Stop at: $(date -r "$stop_epoch" 2>/dev/null || date -d "@$stop_epoch" 2>/dev/null || echo "$stop_epoch")"
    echo "($need_hr hours remaining for 8h/day average over $num_days day(s))"
    ;;
  workalias)
    if [ $# -lt 2 ]; then
      echo "Usage: ts workalias <pattern> <replacement>" >&2
      echo "  pattern: string or regular expression matching activity text for this week." >&2
      echo "  replacement: string to substitute for matched activity." >&2
      exit 1
    fi
    pattern=$1
    replacement=$2
    if [ ! -f "$TIMESHEET" ]; then
      echo "ts workalias: no timesheet data found." >&2
      exit 1
    fi
    today_ymd=$(date +%Y-%m-%d)
    today_start=$(date -d "${today_ymd} 00:00:00" +%s 2>/dev/null) || today_start=$(date -j -f "%Y-%m-%d %H:%M:%S" "${today_ymd} 00:00:00" +%s 2>/dev/null)
    dow=$(date +%w)
    week_start=$((today_start - dow * 86400))
    week_end=$((week_start + 7 * 86400 - 1))
    awk -F'|' -v pat="$pattern" -v repl="$replacement" -v ws="$week_start" -v we="$week_end" '
      /^START\|/ {
        if ($2 >= ws && $2 <= we && $3 ~ pat) {
          new = "START|" $2 "|" repl
          print NR; print $0; print new
        }
      }
    ' "$TIMESHEET" > /tmp/workalias_matches_$$
    match_file=/tmp/workalias_matches_$$
    if [ ! -s "$match_file" ]; then
      rm -f "$match_file"
      echo "ts workalias: no activities matching \"$pattern\" found for this week." >&2
      exit 1
    fi
    replace_nums=""
    while read -r line_num && read -r orig && read -r new_line; do
      echo "Original: $orig"
      echo "Replaced: $new_line"
      printf "Replace (y/n) "
      read -r answer < /dev/tty
      case "$answer" in
        [yY]) replace_nums="$replace_nums $line_num" ;;
        *) ;;
      esac
    done < "$match_file"
    rm -f "$match_file"
    if [ -z "$replace_nums" ]; then
      exit 0
    fi
    temp=$(mktemp)
    trap 'rm -f "$temp"' EXIT
    awk -F'|' -v pat="$pattern" -v repl="$replacement" -v ws="$week_start" -v we="$week_end" -v nums="$replace_nums" '
      BEGIN {
        n = split(nums, a, " ")
        for (i = 1; i <= n; i++) if (a[i] != "") replace[a[i]] = 1
      }
      /^START\|/ {
        if ($2 >= ws && $2 <= we && $3 ~ pat && replace[NR]) {
          print "START|" $2 "|" repl
          next
        }
      }
      { print }
    ' "$TIMESHEET" > "$temp" && mv "$temp" "$TIMESHEET"
    ;;
  install)
    set -e
    SCRIPT_DIR=$TS_DIR
    [ -n "$2" ] && SCRIPT_DIR=$2
    if [ -n "$1" ]; then
      DEST="$1"
      if [ ! -d "$DEST" ]; then
        mkdir -p "$DEST" || {
          echo "ts install: cannot create directory $DEST" >&2
          exit 1
        }
      fi
      if [ ! -w "$DEST" ]; then
        echo "ts install: directory is not writable: $DEST" >&2
        exit 1
      fi
    else
      DEST=""
      old_IFS=$IFS
      IFS=:
      for dir in $PATH; do
        [ -z "$dir" ] && dir="."
        if [ -d "$dir" ] && [ -w "$dir" ]; then
          DEST=$dir
          break
        fi
      done
      IFS=$old_IFS
      if [ -z "$DEST" ]; then
        echo "ts install: no writable directory on PATH. Specify an installation directory." >&2
        exit 1
      fi
    fi
    if [ ! -f "$SCRIPT_DIR/ts" ]; then
      echo "ts install: missing $SCRIPT_DIR/ts" >&2
      exit 1
    fi
    cp "$SCRIPT_DIR/ts" "$DEST/ts"
    chmod +x "$DEST/ts"
    echo "Installed $DEST/ts"
    echo "Done. ts is in $DEST and executable."
    ;;
  rotate)
    do_rotate || exit 1
    ;;
  *)
    echo "Usage: ts <command> [args...]" >&2
    echo "  start | stop | list | started | timeoff | workalias | install | rotate" >&2
    exit 1
    ;;
esac
