#!/bin/sh
# Cloud metadata. Checks the response, since the gateway answers port 80 itself.
. "$(dirname "$0")/lib.sh"
check_http metadata.ipv4 169.254.169.254 80
