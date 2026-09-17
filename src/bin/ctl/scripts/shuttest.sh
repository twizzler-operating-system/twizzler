#!/usr/bin/env brush
# Verify that init's state watcher, not this program, is what takes the machine down.
#
# `ctl shutdown` only sets the monitor's SHUTDOWN flag. This script then blocks forever, so
# init's autostart path cannot be the thing that ends the boot -- if the guest goes down, the
# watcher did it.
echo "== requesting shutdown through the monitor"
ctl shutdown
echo "== requested; sleeping (the watcher should take us down)"
i=0
while [ "$i" -lt 60 ]; do
	sleep 1
	i=$((i + 1))
done
echo "== FAILED: still alive after 60s"
