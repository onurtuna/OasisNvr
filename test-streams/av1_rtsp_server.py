#!/usr/bin/env python3
"""Minimal RTSP server exposing one AV1-encoded synthetic test stream.

Real IP cameras act as RTSP *servers* (the NVR connects to them as a
client), not as RTSP *pushers*. GStreamer's own client-push element
(rtspclientsink) isn't compiled into Debian's gst-plugins-bad package, so
this mimics a real camera instead: it hosts the stream itself via
gst-rtsp-server, and OasisNvr's rtspsrc connects to it exactly like it
would to a physical AV1 camera.
"""
import gi

gi.require_version("Gst", "1.0")
gi.require_version("GstRtspServer", "1.0")
from gi.repository import Gst, GstRtspServer, GLib

Gst.init(None)

LAUNCH = (
    "( videotestsrc pattern=smpte is-live=true "
    "! video/x-raw,width=1280,height=720,framerate=25/1 "
    "! av1enc cpu-used=8 end-usage=cbr target-bitrate=2000 keyframe-max-dist=50 "
    "! av1parse ! rtpav1pay name=pay0 pt=96 )"
)

server = GstRtspServer.RTSPServer()
server.set_service("8554")

factory = GstRtspServer.RTSPMediaFactory()
factory.set_launch(LAUNCH)
factory.set_shared(True)

mounts = server.get_mount_points()
mounts.add_factory("/cam4", factory)

server.attach(None)
print("AV1 test camera serving at rtsp://0.0.0.0:8554/cam4", flush=True)
GLib.MainLoop().run()
