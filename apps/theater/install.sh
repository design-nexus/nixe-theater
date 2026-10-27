#!/usr/bin/env bash
# Install theater: the Nyx suite installer, for this app. Options pass through
# (see ../../install.sh), e.g. ./install.sh --default
here=$(cd "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
exec "$here/../../install.sh" theater "$@"
