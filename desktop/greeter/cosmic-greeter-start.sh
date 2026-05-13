#!/bin/sh
rm -rf /run/cosmic-greeter/cosmic/com.clawos.SettingsDaemon/v1/* > /dev/null 2>&1
exec cosmic-comp cosmic-greeter > /dev/null 2>&1