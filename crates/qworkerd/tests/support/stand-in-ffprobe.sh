#!/bin/sh
# STAND-IN for the media supervisor's tests. It is not ffprobe: it answers every call with the same
# description of an 8-second 640x360 file with one video and one audio stream.
echo '{"format":{"duration":"8.000000","format_name":"mov,mp4,m4a,3gp,3g2,mj2"},"streams":[{"codec_type":"video","width":640,"height":360},{"codec_type":"audio"}]}'
