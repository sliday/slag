#!/usr/bin/env bash
# Render the slag.dev hero video and every file the site serves from it.
#
#   ./render.sh                     frames, webm, mp4, poster, contact sheets
#   POSTER_FRAME=180 ./render.sh    take the poster from another frame
#
# Committed outputs:  ../public/media/slag-hero.webm   (VP9, <= 3 MiB)
#                     ../public/media/slag-hero.mp4    (H.264, faststart, <= 4 MiB)
#                     ../public/media/slag-hero-poster.webp
# Scratch (ignored):  out/frames/, out/contact.png (one frame per second),
#                     out/contact-390.png (the same frames at phone width)
set -euo pipefail

cd "$(dirname "$0")"
VIDEO_DIR="$PWD"
MEDIA="$VIDEO_DIR/../public/media"
OUT="$VIDEO_DIR/out"
FRAMES="$OUT/frames"
FPS=30
POSTER_FRAME="${POSTER_FRAME:-0}"
WEBM_MAX=$((3 * 1024 * 1024))
MP4_MAX=$((4 * 1024 * 1024))

need() { command -v "$1" >/dev/null || { echo "render.sh: $1 is required" >&2; exit 1; }; }
need node
need ffmpeg
need ffprobe
need cwebp
need magick

# The video palette mirrors website/src/main.css. Stop before rendering a
# video in colors the site no longer uses.
check_palette() {
  local css="$VIDEO_DIR/../src/main.css" ts="$VIDEO_DIR/src/theme.ts" bad=0 pair var key a b
  for pair in slag-cold:cold slag-warm:warm slag-hot:hot slag-bright:bright slag-pure:pure \
    background0:bg0 background1:bg1 background2:bg2 background3:bg3 \
    foreground0:fg0 foreground1:fg1 foreground2:fg2; do
    var="${pair%%:*}"
    key="${pair##*:}"
    a=$(grep -Eo -- "--$var: *#[0-9a-fA-F]{6}" "$css" | grep -Eo '#[0-9a-fA-F]{6}' | tr 'A-F' 'a-f')
    b=$(grep -Eo "^ *$key: '#[0-9a-fA-F]{6}'" "$ts" | grep -Eo '#[0-9a-fA-F]{6}' | tr 'A-F' 'a-f')
    if [ -z "$a" ] || [ "$a" != "$b" ]; then
      echo "palette drift: --$var is '${a}' in src/main.css, $key is '${b}' in video/src/theme.ts" >&2
      bad=1
    fi
  done
  return $bad
}
check_palette

[ -d node_modules ] || npm ci --no-audit --no-fund

echo "▸ frames"
mkdir -p "$FRAMES" "$MEDIA"
find "$FRAMES" -maxdepth 1 -name '*.png' -delete
npx remotion render src/index.ts Hero "$FRAMES" --sequence --image-format=png --log=error
COUNT=$(find "$FRAMES" -maxdepth 1 -name 'element-*.png' | wc -l | tr -d ' ')
LAST=$((COUNT - 1))
DIGITS=${#LAST}
PATTERN="$FRAMES/element-%0${DIGITS}d.png"
frame() { printf "%s/element-%0${DIGITS}d.png" "$FRAMES" "$1"; }

# Chrome renders sRGB; tag the encodes BT.709 so browsers do not shift the
# palette.
COLOR=(-vf "scale=out_color_matrix=bt709:out_range=tv,format=yuv420p"
  -color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range tv)

size_of() { stat -f%z "$1" 2>/dev/null || stat -c%s "$1"; }

echo "▸ webm (VP9, two-pass constant quality)"
crf=31
while :; do
  ffmpeg -v error -y -framerate "$FPS" -i "$PATTERN" "${COLOR[@]}" -c:v libvpx-vp9 -b:v 0 -crf "$crf" \
    -row-mt 1 -tile-columns 2 -deadline good -cpu-used 1 -g 300 -an \
    -pass 1 -passlogfile "$OUT/vp9" -f null /dev/null
  ffmpeg -v error -y -framerate "$FPS" -i "$PATTERN" "${COLOR[@]}" -c:v libvpx-vp9 -b:v 0 -crf "$crf" \
    -row-mt 1 -tile-columns 2 -deadline good -cpu-used 1 -g 300 -an \
    -pass 2 -passlogfile "$OUT/vp9" "$OUT/slag-hero.webm"
  bytes=$(size_of "$OUT/slag-hero.webm")
  echo "  crf $crf: $bytes bytes"
  [ "$bytes" -le "$WEBM_MAX" ] && break
  crf=$((crf + 3))
  [ "$crf" -le 52 ] || { echo "webm stays over budget" >&2; exit 1; }
done
cp "$OUT/slag-hero.webm" "$MEDIA/slag-hero.webm"

echo "▸ mp4 (H.264 high, faststart)"
crf=20
while :; do
  ffmpeg -v error -y -framerate "$FPS" -i "$PATTERN" "${COLOR[@]}" -c:v libx264 -preset veryslow -tune animation \
    -crf "$crf" -profile:v high -level 4.0 -g 300 -movflags +faststart -an "$OUT/slag-hero.mp4"
  bytes=$(size_of "$OUT/slag-hero.mp4")
  echo "  crf $crf: $bytes bytes"
  [ "$bytes" -le "$MP4_MAX" ] && break
  crf=$((crf + 2))
  [ "$crf" -le 34 ] || { echo "mp4 stays over budget" >&2; exit 1; }
done
cp "$OUT/slag-hero.mp4" "$MEDIA/slag-hero.mp4"

echo "▸ poster (frame $POSTER_FRAME)"
cwebp -quiet -q 82 -m 6 "$(frame "$POSTER_FRAME")" -o "$MEDIA/slag-hero-poster.webp"

echo "▸ contact sheets"
SECS=$((COUNT / FPS))
ROWS6=$(((SECS + 5) / 6))
ROWS4=$(((SECS + 3) / 4))
ffmpeg -v error -y -framerate "$FPS" -i "$PATTERN" \
  -vf "select='not(mod(n\,$FPS))',scale=480:-2,tile=6x$ROWS6:padding=6:color=0x2b3040" \
  -frames:v 1 "$OUT/contact.png"
ffmpeg -v error -y -framerate "$FPS" -i "$PATTERN" \
  -vf "select='not(mod(n\,$FPS))',scale=390:-2,tile=4x$ROWS4:padding=6:color=0x2b3040" \
  -frames:v 1 "$OUT/contact-390.png"

# The loop seam: the last frame should differ from the first no more than
# two neighbouring frames do.
seam=$(magick compare -metric RMSE "$(frame "$LAST")" "$(frame 0)" null: 2>&1 | awk '{print $1}' || true)
step=$(magick compare -metric RMSE "$(frame $((LAST - 1)))" "$(frame "$LAST")" null: 2>&1 | awk '{print $1}' || true)

echo
echo "▸ summary"
for f in "$MEDIA/slag-hero.webm" "$MEDIA/slag-hero.mp4"; do
  ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,width,height,r_frame_rate:format=duration,size \
    -of default=noprint_wrappers=1 "$f" | tr '\n' ' '
  audio=$(ffprobe -v error -select_streams a -show_entries stream=index -of csv=p=0 "$f" | wc -l | tr -d ' ')
  echo " audio_streams=$audio  $(basename "$f")"
done
echo "poster  $(size_of "$MEDIA/slag-hero-poster.webp") bytes  $(basename "$MEDIA/slag-hero-poster.webp")"
echo "loop seam RMSE last→first: $seam   (last two frames: $step)"
echo "contact sheets: $OUT/contact.png  $OUT/contact-390.png"
