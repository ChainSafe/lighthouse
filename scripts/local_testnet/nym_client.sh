#!/usr/bin/env bash

#
# Starts a nym-client.
#

set -Eeuo pipefail

# Get options
while getopts "d:sh" flag; do
  case "${flag}" in
    h)
       echo "Start a nym client"
       echo
       echo "usage: $0 [WS-PORT]"
       exit
       ;;
  esac
done

# Get positional arguments
nym_client_port=${@:$OPTIND+0:1}

# init client with port as id
nym-client init --id "$nym_client_port"

# run client as id port
exec nym-client run --id "$nym_client_port" \
    --host '0.0.0.0' \
    --port "$nym_client_port"
