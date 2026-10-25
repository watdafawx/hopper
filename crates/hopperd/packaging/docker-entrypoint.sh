#!/bin/sh
# Starts Hopper Server in the container. On `docker stop` it stops the
# panel, then the Minecraft servers (Java saves their worlds on SIGTERM),
# and waits for them before the container exits. Give the container time
# for that: --stop-timeout 120 (compose: stop_grace_period: 2m).
set -u

# Every Java in the container is a server (or an installer) hopperd started.
java_pids() {
  for d in /proc/[0-9]*; do
    case "$(cat "$d/comm" 2>/dev/null)" in
      java|java.exe) grep -q '^State:.*Z' "$d/status" 2>/dev/null || echo "${d#/proc/}" ;;
    esac
  done
}

stop_servers() {
  pids=$(java_pids)
  [ -z "$pids" ] && return
  echo "hopperd-docker: stopping the Minecraft servers:" $pids
  kill -TERM $pids 2>/dev/null
  i=0
  while [ $i -lt 110 ] && [ -n "$(java_pids)" ]; do
    sleep 1
    i=$((i + 1))
  done
  [ -n "$(java_pids)" ] && echo "hopperd-docker: servers still running after ${i}s:" $(java_pids)
}

mkdir -p /hopper/servers
if [ -f /data/hopperd.toml ] && [ ! -f /hopper/.hopper-server/hopperd.toml ]; then
  # An image from before 0.9.1 (volumes /data and /servers): keep running
  # that way. See "Docker" in the README to move to /hopper.
  export HOPPERD_DATA=/data
  set -- --servers-root /servers "$@"
else
  # /hopper is the panel folder: servers in /hopper/servers, the panel's
  # data in /hopper/.hopper-server.
  set -- --home /hopper --servers-root /hopper/servers "$@"
fi

hopperd "$@" &
pid=$!
trap 'kill -INT $pid 2>/dev/null' TERM INT
code=0
while kill -0 $pid 2>/dev/null; do
  wait $pid
  code=$?
done
echo "hopperd-docker: the panel stopped (exit code $code)"
stop_servers
exit $code
