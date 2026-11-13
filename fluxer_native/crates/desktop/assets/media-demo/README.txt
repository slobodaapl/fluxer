Generated fixtures; no third-party or user media.

video.mp4: ffmpeg -f lavfi -i testsrc2=size=480x270:rate=24:duration=3 -an -c:v libx264 -preset ultrafast -crf 28 -pix_fmt yuv420p -movflags +faststart video.mp4
poster.png: ffmpeg -i video.mp4 -frames:v 1 poster.png
tone.ogg: ffmpeg -f lavfi -i sine=frequency=440:sample_rate=48000:duration=3 -ac 2 -c:a libopus -b:a 64k tone.ogg

Only --demo-media serves these bytes on loopback. Automatic fixture playback
uses the silent video; the tone requires an explicit Play action.
