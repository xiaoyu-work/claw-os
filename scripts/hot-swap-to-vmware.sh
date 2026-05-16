#!/usr/bin/env bash
# hot-swap-to-vmware.sh — rebuild one or more claw-os binaries, inject them
# into the qcow2 image, regenerate the VMware Fusion vmdk, and (optionally)
# restart the Fusion VM.
#
# Usage:
#   scripts/hot-swap-to-vmware.sh [options] TARGET [TARGET ...]
#
# TARGETs (default install path /usr/bin/<name>):
#   cos                    — workspace root,        target/release/cos
#   cos-agent-ui           — desktop/agent,         target/release/cos-agent-ui
#   cos-agent-bridge       — desktop/agent,         target/release/cos-agent-bridge
#   cosmic-files           — desktop/files,         target/release/cosmic-files
#   cosmic-edit            — desktop/edit,          target/release/cosmic-edit
#   cosmic-term            — desktop/term,          target/release/cosmic-term
#   cosmic-initial-setup   — desktop/initial-setup, target/release/cosmic-initial-setup
#
# Options:
#   --no-build      Skip cargo build; use whatever is already in target/release
#   --no-stop       Don't try to stop the Fusion VM (script will fail if running)
#   --no-start      Don't restart the VM after the swap
#   --no-convert    Skip qcow2 → vmdk conversion (only inject into qcow2)
#   -h | --help     This message
#
# Environment knobs:
#   ORB_VM          OrbStack VM name (default: clawbuild)
#   QCOW            host path to the qcow2 (default: build/claw-os-vm-arm64.qcow2)
#   VMWAREVM        Fusion bundle dir (default: ~/Virtual Machines.localized/ClawOS.vmwarevm)

set -euo pipefail

ORB_VM=${ORB_VM:-clawbuild}
REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
QCOW=${QCOW:-"$REPO_ROOT/build/claw-os-vm-arm64.qcow2"}
VMWAREVM=${VMWAREVM:-"$HOME/Virtual Machines.localized/ClawOS.vmwarevm"}
VMX="$VMWAREVM/ClawOS.vmx"
VMDK="$VMWAREVM/disk.vmdk"
VMRUN="/Applications/VMware Fusion.app/Contents/Public/vmrun"

DO_BUILD=1
DO_STOP=1
DO_START=1
DO_CONVERT=1
TARGETS=()

usage() {
  sed -n '2,/^set -euo/p' "$0" | sed -n 's/^# \{0,1\}//p' | sed '$d'
  exit "${1:-0}"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --no-build)   DO_BUILD=0 ;;
    --no-stop)    DO_STOP=0 ;;
    --no-start)   DO_START=0 ;;
    --no-convert) DO_CONVERT=0 ;;
    -h|--help)    usage 0 ;;
    -*)           echo "unknown flag: $1" >&2; usage 2 ;;
    *)            TARGETS+=("$1") ;;
  esac
  shift
done

[ ${#TARGETS[@]} -gt 0 ] || { echo "no target specified" >&2; usage 2; }

# target → "<cargo_dir>|<binary>|<install_path>"
target_spec() {
  case "$1" in
    cos)                   echo ".|cos|/usr/bin/cos" ;;
    cos-agent-ui)          echo "desktop/agent|cos-agent-ui|/usr/bin/cos-agent-ui" ;;
    cos-agent-bridge)      echo "desktop/agent|cos-agent-bridge|/usr/bin/cos-agent-bridge" ;;
    cosmic-files)          echo "desktop/files|cosmic-files|/usr/bin/cosmic-files" ;;
    cosmic-edit)           echo "desktop/edit|cosmic-edit|/usr/bin/cosmic-edit" ;;
    cosmic-term)           echo "desktop/term|cosmic-term|/usr/bin/cosmic-term" ;;
    cosmic-initial-setup)  echo "desktop/initial-setup|cosmic-initial-setup|/usr/bin/cosmic-initial-setup" ;;
    *) echo "" ;;
  esac
}

# Validate targets up front
for t in "${TARGETS[@]}"; do
  spec=$(target_spec "$t")
  [ -n "$spec" ] || { echo "unknown target: $t" >&2; usage 2; }
done

[ -f "$QCOW" ] || { echo "qcow2 missing: $QCOW" >&2; exit 1; }

vm_running() {
  "$VMRUN" -T fusion list 2>/dev/null | grep -Fq "$VMX"
}

if [ "$DO_STOP" = 1 ] && [ -f "$VMX" ] && vm_running; then
  echo ">> stopping VMware Fusion VM (soft, then hard)"
  "$VMRUN" -T fusion stop "$VMX" soft 2>/dev/null || true
  for _ in $(seq 1 30); do vm_running || break; sleep 1; done
  if vm_running; then
    "$VMRUN" -T fusion stop "$VMX" hard 2>/dev/null || true
    for _ in $(seq 1 15); do vm_running || break; sleep 1; done
  fi
  if vm_running; then echo "VM refused to stop" >&2; exit 1; fi
fi

if [ "$DO_BUILD" = 1 ]; then
  echo ">> building inside OrbStack VM '$ORB_VM'"
  for t in "${TARGETS[@]}"; do
    IFS='|' read -r cargo_dir bin _ <<<"$(target_spec "$t")"
    echo "   - $t  ($cargo_dir/target/release/$bin)"
  done
  build_script=""
  for t in "${TARGETS[@]}"; do
    IFS='|' read -r cargo_dir bin _ <<<"$(target_spec "$t")"
    build_script+="echo '== build $bin ($cargo_dir) =='; (cd '$cargo_dir' && cargo build --release --bin '$bin') || exit 1; "
  done
  orb -m "$ORB_VM" bash -lc "set -e; cd /Users/jay/workspace/claw-os; $build_script"
fi

echo ">> injecting binaries into qcow2 ($QCOW)"
upload_args=""
chmod_args=""
for t in "${TARGETS[@]}"; do
  IFS='|' read -r cargo_dir bin install <<<"$(target_spec "$t")"
  src="$cargo_dir/target/release/$bin"
  upload_args+=" --upload '$src:$install'"
  chmod_args+=" --run-command 'chmod 0755 $install'"
done
orb -m "$ORB_VM" bash -lc "set -e; cd /Users/jay/workspace/claw-os; \
  virt-customize -a '${QCOW#"$REPO_ROOT"/}' $upload_args $chmod_args"

if [ "$DO_CONVERT" = 1 ] && [ -f "$VMDK" ]; then
  echo ">> qcow2 → vmdk ($VMDK)"
  rm -f "$VMDK"
  qemu-img convert -p -f qcow2 -O vmdk -o subformat=monolithicSparse "$QCOW" "$VMDK"
fi

if [ "$DO_START" = 1 ] && [ -f "$VMX" ]; then
  echo ">> starting VMware Fusion VM"
  "$VMRUN" -T fusion start "$VMX" gui
fi

echo "done."
