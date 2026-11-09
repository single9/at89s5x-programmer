#!/usr/bin/env bash
# One-time Linux setup for the FT232R (FT232RL) backend.
# Installs a udev rule so non-root users in "plugdev" can open the USB device.
# Safe to re-run.
set -euo pipefail

RULE_FILE=/etc/udev/rules.d/99-ft232r.rules
RULE='SUBSYSTEM=="usb", ATTR{idVendor}=="0403", ATTR{idProduct}=="6001", GROUP="plugdev", MODE="0660"'

if [ "$(id -u)" -eq 0 ]; then SUDO=""; else SUDO="sudo"; fi

echo "==> plugdev group"
if getent group plugdev >/dev/null; then
  if id -nG "${SUDO_USER:-$USER}" | tr ' ' '\n' | grep -qx plugdev; then
    echo "    ${SUDO_USER:-$USER} is in plugdev"
  else
    echo "    adding ${SUDO_USER:-$USER} to plugdev (log out and back in afterwards)"
    $SUDO usermod -aG plugdev "${SUDO_USER:-$USER}"
  fi
else
  echo "    group 'plugdev' does not exist; create it or edit GROUP= in $RULE_FILE" >&2
  exit 1
fi

echo "==> udev rule $RULE_FILE"
if [ -f "$RULE_FILE" ] && grep -qxF "$RULE" "$RULE_FILE"; then
  echo "    already installed"
else
  echo "$RULE" | $SUDO tee "$RULE_FILE" >/dev/null
  $SUDO udevadm control --reload-rules
  $SUDO udevadm trigger
  echo "    installed"
fi

echo
echo "Done. Unplug and re-plug the FT232RL, then try:"
echo "  at89s5x-isp info"
