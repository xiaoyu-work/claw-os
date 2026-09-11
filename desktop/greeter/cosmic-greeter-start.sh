#!/bin/sh
rm -rf /run/cosmic-greeter/cosmic/com.clawos.SettingsDaemon/v1/* > /dev/null 2>&1
exec /usr/local/bin/claw-display-session --exec /usr/bin/cosmic-greeter