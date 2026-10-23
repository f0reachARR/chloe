#!/bin/sh
# public/icons の SVG から、PWA と iOS のホーム画面に使う PNG を作る (inkscape が要る)。
# 作った PNG はリポジトリに置くので、アイコンを変えたときだけ実行する。
set -eu
cd "$(dirname "$0")/../public/icons"
inkscape icon.svg -w 192 -h 192 -o icon-192.png
inkscape icon.svg -w 512 -h 512 -o icon-512.png
inkscape maskable.svg -w 512 -h 512 -o maskable-512.png
inkscape maskable.svg -w 180 -h 180 -o apple-touch-icon.png
