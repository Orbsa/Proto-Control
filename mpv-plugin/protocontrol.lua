-- Proto-Control MPV plugin
--
-- Ensures the JSON IPC server is available for the proto-control daemon.
-- Install: symlink or copy to ~/.config/mpv/scripts/protocontrol.lua
--
-- The daemon connects to this socket to control playback speed and pause.

local SOCKET_PATH = "/tmp/proto-control-mpv.sock"

-- Enable the IPC server if not already configured
local current = mp.get_property("input-ipc-server")
if not current or current == "" then
    mp.set_property("input-ipc-server", SOCKET_PATH)
    mp.msg.info("Proto-Control: IPC server enabled at " .. SOCKET_PATH)
else
    mp.msg.info("Proto-Control: IPC server already at " .. current)
end
