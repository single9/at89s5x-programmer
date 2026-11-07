#!/usr/bin/env bash
# One-time Linux setup for the FT232R (FT232RL) backend.
# Installs a udev rule so non-root users in "plugdev" can open the USB device,
# and checks the build prerequisites. Safe to re-run.
set -euo pipefail

RULE_FILE=/etc/udev/rules.d/99-ft232r.rules
RULE='SUBSYSTEM=="usb", ATTR{idVendor}=="0403", ATTR{idProduct}=="6001", GROUP="plugdev", MODE="0660"'

if [ "$(id -u)" -eq 0 ]; then SUDO=""; else SUDO="sudo"; fi

echo "==> libusb development files"
if pkg-config --exists libusb-1.0 2>/dev/null; then
  echo "    found libusb-1.0 $(pkg-config --modversion libusb-1.0)"
else
  echo "    MISSING. Install it first, e.g.: sudo apt install libusb-1.0-0-dev pkg-config" >&2
  exit 1
fi

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
echo "  cargo run -- --adapter ft232r info"
