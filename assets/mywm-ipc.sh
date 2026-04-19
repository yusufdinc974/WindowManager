#!/bin/bash
# mywm-ipc.sh — Send a JSON command to the compositor's IPC socket.
# Usage: mywm-ipc.sh '{"AdjustOpacity":{"value":0.05}}'

SOCK="/tmp/mywm.sock"
MSG="${1:?usage: mywm-ipc.sh '<json>'}"

if [ ! -S "$SOCK" ]; then
    exit 1
fi

if command -v socat >/dev/null 2>&1; then
    echo "$MSG" | socat - UNIX-CONNECT:"$SOCK" 2>/dev/null
elif command -v python3 >/dev/null 2>&1; then
    python3 -c "
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect('$SOCK')
s.sendall(sys.argv[1].encode())
s.close()
" "$MSG" 2>/dev/null
elif command -v nc >/dev/null 2>&1; then
    echo "$MSG" | nc -U "$SOCK" -w1 2>/dev/null
else
    # Last resort: bash /dev/tcp doesn't support unix sockets,
    # so try perl
    perl -e "
use IO::Socket::UNIX;
my \$s = IO::Socket::UNIX->new(Peer=>'$SOCK', Type=>SOCK_STREAM) or exit 1;
print \$s '$MSG';
close \$s;
" 2>/dev/null
fi