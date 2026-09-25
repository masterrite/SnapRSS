#!/bin/bash
export DISPLAY=:99
import -window root "/tmp/$1.png" && convert "/tmp/$1.png" -resize "${2:-1150}x" "/tmp/$1_s.png" && echo "captured /tmp/$1_s.png"
