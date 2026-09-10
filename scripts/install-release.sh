#!/bin/sh
# Install taimux from a GitHub release.
#
# taimux is a binary and not every host that runs it can build one, so the
# release carries ONE statically linked x86_64 artefact under a stable name and
# this fetches it. Static is what makes one file serve every machine: hosts
# differ in glibc, and a build on a newer one does not run on an older.
#
# It installs $DIR/taimux. On a development machine that launcher normally
# points into the checkout instead, so running this there takes the working-tree
# build out of the picker until `just install` points it back. A configuration
# manager should only call this when there is nothing runnable at the launcher,
# so it cannot do that by accident.
#
# usage:
#   install-release.sh [--force] [--tag vX.Y.Z] [--dir DIR] [--repo OWNER/NAME]
#
#   --force   download even if the installed version already matches
#   --tag     a specific release rather than the latest
#   --dir     where to install (default ~/.local/bin)
#   --repo    another repository (default pdecat/taimux, or $TAIMUX_REPO)
#
# Idempotent and quiet: already on that version, it downloads nothing and says
# so, which is what makes it safe to run on every configuration-management pass.
#
# No credential is needed for a public repository, and none is looked for. Set
# $TAIMUX_GH_TOKEN, $GH_TOKEN or $GITHUB_TOKEN to authenticate anyway, which is
# worth doing from CI (the anonymous GitHub API allows 60 requests an hour per
# IP, and a shared runner address can exhaust that) and is required if --repo
# points at a private fork. The token goes to curl through a config file on
# stdin, never in argv, so it does not show up in `ps` for anyone watching.
set -eu

REPO=${TAIMUX_REPO:-pdecat/taimux}
# Overridable so the test suite can point the whole thing at a release it
# controls; nothing else has a reason to change it.
ASSET=${TAIMUX_ASSET:-taimux-x86_64-linux-musl}
DIR=${TAIMUX_BIN_DIR:-$HOME/.local/bin}
TAG=
FORCE=0

usage() {
  cat <<'EOF'
usage: install-release.sh [--force] [--tag vX.Y.Z] [--dir DIR] [--repo OWNER/NAME]

  --force   download even if the installed version already matches
  --tag     a specific release rather than the latest
  --dir     where to install (default ~/.local/bin)
  --repo    another repository (default pdecat/taimux, or $TAIMUX_REPO)
EOF
}

while [ $# -gt 0 ]; do
  case $1 in
    --force) FORCE=1 ;;
    --tag) TAG=${2:?--tag needs a value}; shift ;;
    --dir) DIR=${2:?--dir needs a value}; shift ;;
    --repo) REPO=${2:?--repo needs a value}; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "install-release.sh: no such option: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

for tool in curl jq; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "install-release.sh: needs $tool" >&2
    exit 1
  }
done

# The credential, if there is one. Empty is the normal answer.
token=
for v in "${TAIMUX_GH_TOKEN:-}" "${GH_TOKEN:-}" "${GITHUB_TOKEN:-}"; do
  if [ -n "$v" ]; then
    token=$v
    break
  fi
done

# curl, with the Authorization header arriving on stdin rather than in argv, so
# a token never reaches the process table.
fetch() {
  if [ -n "$token" ]; then
    printf 'header = "Authorization: Bearer %s"\n' "$token" |
      curl --config - --fail --silent --show-error --location "$@"
  else
    curl --fail --silent --show-error --location "$@"
  fi
}

# The release to read, and the one hook the test suite needs: pointed at a
# `file://` fixture whose asset url is also a `file://`, every branch below runs
# with no network and no GitHub at all. curl treats both schemes the same.
if [ -n "${TAIMUX_RELEASE_URL:-}" ]; then
  url=$TAIMUX_RELEASE_URL
elif [ -n "$TAG" ]; then
  url=https://api.github.com/repos/$REPO/releases/tags/$TAG
else
  url=https://api.github.com/repos/$REPO/releases/latest
fi

json=$(fetch -H 'Accept: application/vnd.github+json' "$url" 2>/dev/null) || {
  echo "install-release.sh: cannot read $url" >&2
  echo "install-release.sh: check the tag, or --repo, or whether the network is" >&2
  echo "install-release.sh: reachable. A private repository answers a missing" >&2
  echo "install-release.sh: credential the same way it answers a missing release," >&2
  echo "install-release.sh: so set \$GH_TOKEN if --repo points at one." >&2
  exit 1
}

tag=$(printf '%s' "$json" | jq -r '.tag_name // empty')
# The ASSET api url rather than browser_download_url, because it works for a
# private --repo too and needs no branching: with the octet-stream Accept below
# the API returns the bytes, and without it, the asset's JSON metadata. curl
# drops the Authorization header on the cross-host redirect to storage, which is
# what storage wants.
asset=$(printf '%s' "$json" | jq -r --arg n "$ASSET" \
  '.assets[]? | select(.name == $n) | .url')

[ -n "$tag" ] || { echo "install-release.sh: release has no tag_name" >&2; exit 1; }
if [ -z "$asset" ]; then
  echo "install-release.sh: release $tag has no asset named $ASSET" >&2
  printf '%s' "$json" | jq -r '"  it has: " + ([.assets[]?.name] | join(", "))' >&2
  exit 1
fi

want=${tag#v}
have=
if [ -x "$DIR/taimux" ]; then
  have=$("$DIR/taimux" version 2>/dev/null | awk '{print $2}')
fi
if [ "$FORCE" = 0 ] && [ -n "$have" ] && [ "$have" = "$want" ]; then
  echo "taimux $have is already installed at $DIR/taimux"
  exit 0
fi

mkdir -p "$DIR"
# Beside the target, so the move is a rename within one filesystem and no
# partial download is ever visible at the real path. Renaming over the running
# binary is fine on Linux: the open file keeps its inode.
tmp=$DIR/.taimux.download.$$
trap 'rm -f "$tmp"' EXIT INT TERM

echo "taimux: fetching $tag ($ASSET) from $REPO"
fetch -H 'Accept: application/octet-stream' -o "$tmp" "$asset"
chmod +x "$tmp"

# It has to run before it is installed. This is the check that catches a
# truncated download, the wrong architecture, and an artefact that was linked
# dynamically and cannot resolve a libc here, all of which otherwise show up
# later as a picker that does nothing.
got=$("$tmp" version 2>/dev/null | awk '{print $2}') || got=
if [ -z "$got" ]; then
  echo "install-release.sh: the downloaded binary does not run here" >&2
  file "$tmp" >&2 || true
  exit 1
fi
if [ "$got" != "$want" ]; then
  echo "install-release.sh: $tag carries a binary saying $got, refusing it" >&2
  exit 1
fi

mv -f "$tmp" "$DIR/taimux"
trap - EXIT INT TERM


if [ -n "$have" ] && [ "$have" != "$got" ]; then
  echo "taimux $have -> $got at $DIR/taimux"
else
  echo "taimux $got at $DIR/taimux"
fi
