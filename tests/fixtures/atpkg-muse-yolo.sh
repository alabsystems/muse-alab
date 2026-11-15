# Copyright 2026 Andrew Yates
# SPDX-License-Identifier: Apache-2.0
#
# atpkg's yolo block for `muse`, as its launcher runs it: the body aterm's
# `sh_yolo_prelude_with_launcher("muse", …)` renders (crates/atpkg/src/platform/mod.rs:1125-1458
# at aterm 617e45d8c95) from the muse row's yolo_* columns
# (crates/atpkg/src/vendor_direct/table.rs:628-673) and the "muse" arm of its word scan
# (crates/atpkg/src/yolo.rs:149-152). Copied byte for byte from the `agents/muse` twin atpkg laid
# on 2026-11-13, from the line after the `--no-harness` strip (which stays the launcher's: it
# decides whether the harness runs at all) to the end of the stderr line; the twin's `exec` line
# that follows names one machine's store and is left out.
#
# src/launcher_tests.rs runs it as a shell function on a corpus of argvs and holds
# `muse_alab::launcher::yolo` to what it prints and the argv it leaves.
__atpkg_yolo=0
__atpkg_posture=0
__atpkg_have_word=0
__atpkg_word=
__atpkg_word_at=0
__atpkg_word_ok=1
__atpkg_p0=0
__atpkg_p1=0
__atpkg_p2=0
__atpkg_p3=0
for __atpkg_a in "$@"; do
  case "$__atpkg_a" in
    --) break ;;
    --yolo) __atpkg_yolo=1 ;;
    '--disable-approval') __atpkg_p0=1; __atpkg_posture=1 ;;
    '--disable-sandbox') __atpkg_p1=1; __atpkg_posture=1 ;;
    '--trust-workspace') __atpkg_p2=1; __atpkg_posture=1 ;;
    '--disable-request-user-input') __atpkg_p3=1; __atpkg_posture=1 ;;
    '--approval-mode'|'--approval-mode='*|'--approval-mode='*|'--permission-profile'|'--permission-profile='*|'--permission-profile='*|'--approval-judge'|'--approval-judge='*|'--approval-judge='*|'--sandbox-network'|'--sandbox-network='*|'--sandbox-network='*|'--disable-write'|'--disable-write='*|'--disable-shell'|'--disable-shell='*|'--config'|'--config='*|'--config='*|'-c'*) __atpkg_posture=1 ;;
  esac
done
__atpkg_at=0
__atpkg_value=0
for __atpkg_a in "$@"; do
  __atpkg_at=$((__atpkg_at + 1))
  if [ "$__atpkg_a" = "--" ]; then break; fi
  if [ "$__atpkg_value" = 1 ]; then __atpkg_value=0; continue; fi
  case "$__atpkg_a" in
    '--yolo'|'--disable-approval'|'--disable-sandbox'|'--trust-workspace'|'--disable-request-user-input') ;;
    '--model') __atpkg_value=1 ;;
    '--model'|'--model='*) ;;
    -*) __atpkg_word_ok=0; break ;;
    *) __atpkg_have_word=1; __atpkg_word=$__atpkg_a; __atpkg_word_at=$__atpkg_at; break ;;
  esac
done
if [ "$__atpkg_value" = 1 ]; then __atpkg_word_ok=0; fi
__atpkg_after=0
if [ "$__atpkg_have_word" = 1 ]; then
  case "$__atpkg_word" in
    'exec'|'resume') __atpkg_after=1 ;;
  esac
fi
__atpkg_say=
if [ "$__atpkg_yolo" = 1 ] && [ "$__atpkg_word_ok" = 1 ]; then
  __atpkg_say="aterm: --yolo: running 'muse'"
  __atpkg_stop=0
  __atpkg_at=0
  __atpkg_pending=0
  __atpkg_n=$#
  while [ "$__atpkg_n" -gt 0 ]; do
    __atpkg_n=$((__atpkg_n - 1))
    __atpkg_at=$((__atpkg_at + 1))
    if [ "$__atpkg_stop" = 0 ] && [ "$1" = "--yolo" ]; then
      shift
      if [ "$__atpkg_after" = 1 ] && [ "$__atpkg_at" -lt "$__atpkg_word_at" ]; then
        __atpkg_pending=1
      else
        if [ "$__atpkg_p0" = 0 ]; then __atpkg_p0=1; set -- "$@" '--disable-approval'; fi
        if [ "$__atpkg_p1" = 0 ]; then __atpkg_p1=1; set -- "$@" '--disable-sandbox'; fi
        if [ "$__atpkg_p2" = 0 ]; then __atpkg_p2=1; set -- "$@" '--trust-workspace'; fi
        if [ "$__atpkg_p3" = 0 ]; then __atpkg_p3=1; set -- "$@" '--disable-request-user-input'; fi
      fi
    else
      if [ "$1" = "--" ]; then __atpkg_stop=1; fi
      set -- "$@" "$1"
      shift
      if [ "$__atpkg_pending" = 1 ] && [ "$__atpkg_at" = "$__atpkg_word_at" ]; then
        __atpkg_pending=0
          if [ "$__atpkg_p0" = 0 ]; then __atpkg_p0=1; set -- "$@" '--disable-approval'; fi
          if [ "$__atpkg_p1" = 0 ]; then __atpkg_p1=1; set -- "$@" '--disable-sandbox'; fi
          if [ "$__atpkg_p2" = 0 ]; then __atpkg_p2=1; set -- "$@" '--trust-workspace'; fi
          if [ "$__atpkg_p3" = 0 ]; then __atpkg_p3=1; set -- "$@" '--disable-request-user-input'; fi
      fi
    fi
  done
elif [ "$__atpkg_posture" = 0 ] && [ "$__atpkg_word_ok" = 1 ]; then
  if [ "$__atpkg_have_word" = 1 ]; then
    case "$__atpkg_word" in
      'resume'|'config'|'export'|'trace'|'model-profile'|'skills'|'voice'|'plugins'|'sandbox'|'schema'|'serve'|'session-message'|'mcp'|'auth'|'login'|'logout'|'usage'|'init') ;;
      *) __atpkg_say="aterm: yolo (default): running 'muse'" ;;
    esac
  else
    __atpkg_say="aterm: yolo (default): running 'muse'"
  fi
  if [ -n "$__atpkg_say" ] && [ "$__atpkg_after" = 0 ]; then
    set -- '--disable-approval' '--disable-sandbox' '--trust-workspace' '--disable-request-user-input' "$@"
  elif [ -n "$__atpkg_say" ]; then
    __atpkg_n=$#
    __atpkg_at=0
    while [ "$__atpkg_n" -gt 0 ]; do
      __atpkg_n=$((__atpkg_n - 1))
      __atpkg_at=$((__atpkg_at + 1))
      set -- "$@" "$1"
      if [ "$__atpkg_at" = "$__atpkg_word_at" ]; then
        set -- "$@" '--disable-approval' '--disable-sandbox' '--trust-workspace' '--disable-request-user-input'
      fi
      shift
    done
  fi
fi
if [ -n "$__atpkg_say" ]; then
  printf '%s' "$__atpkg_say" 1>&2
  for __atpkg_a in "$@"; do
    printf " '" 1>&2
    __atpkg_rest=$__atpkg_a
    while case "$__atpkg_rest" in *"'"*) true ;; *) false ;; esac; do
      printf '%s' "${__atpkg_rest%%"'"*}" 1>&2
      printf '%s' "'\\''" 1>&2
      __atpkg_rest=${__atpkg_rest#*"'"}
    done
    printf '%s' "$__atpkg_rest" 1>&2
    printf "'" 1>&2
  done
  printf '\n' 1>&2
fi
