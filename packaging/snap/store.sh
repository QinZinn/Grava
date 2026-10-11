#!/bin/sh
# Run the official Snapcraft CLI on Fedora without installing snapd.
set -eu
umask 077

# Canonical's 8_core24 image, verified as Snapcraft 8.11.1.
image='ghcr.io/canonical/snapcraft@sha256:0443273552768a3230c2ede3aa47e567da0242bfbb0a7bb1283093208c404a0c'
credential_dir="${XDG_STATE_HOME:-$HOME/.local/state}/grava/snap-store"

usage() {
    printf '%s\n' 'Usage: packaging/snap/store.sh login | status | upload FILE.snap' >&2
    exit 2
}

[ "$#" -ge 1 ] || usage
action=$1
shift
case "$action" in
    login)
        [ "$#" -eq 0 ] || usage
        if [ ! -t 0 ] || [ ! -t 1 ]; then
            printf '%s\n' 'Run login yourself in an interactive terminal.' >&2
            exit 1
        fi
        mkdir -p "$credential_dir"
        chmod 0700 "$credential_dir"
        login_dir=$(mktemp -d "$credential_dir/login.XXXXXX")
        trap '[ ! -f "$login_dir/store.json" ] || rm -- "$login_dir/store.json"; rmdir -- "$login_dir"' EXIT
        expires=$(date -u -d '+1 day' '+%Y-%m-%dT%H:%M:%SZ')
        podman run --rm -it --entrypoint /bin/snapcraft \
            --volume "$login_dir:/credentials:Z" "$image" \
            export-login /credentials/store.json --snaps=grava --channels=edge \
            --acls=package_access,package_push,package_release --expires="$expires"
        chmod 0600 "$login_dir/store.json"
        mv -- "$login_dir/store.json" "$credential_dir/store.json"
        printf '%s\n' 'Saved credentials for grava/edge only; they expire in one day.'
        ;;
    status|upload)
        if [ "$action" = status ]; then
            [ "$#" -eq 0 ] || usage
        else
            [ "$#" -eq 1 ] || usage
            [ -f "$1" ] || { printf '%s\n' 'Snap artifact not found.' >&2; exit 1; }
            artifact=$(realpath -- "$1")
            artifact_dir=$(dirname -- "$artifact")
            artifact_name=$(basename -- "$artifact")
            case "$artifact_name" in grava_*.snap) ;; *) usage ;; esac
        fi
        if [ ! -s "$credential_dir/store.json" ]; then
            printf '%s\n' 'First run: packaging/snap/store.sh login' >&2
            exit 1
        fi
        if [ "$action" = status ]; then
            set -- status grava
        else
            set -- upload "/artifacts/$artifact_name" --release=edge
        fi
        if [ "$action" = upload ]; then
            podman run --rm --entrypoint /bin/sh \
                --volume "$credential_dir:/credentials:ro,Z" \
                --volume "$artifact_dir:/artifacts:ro,Z" "$image" -ec \
                'export SNAPCRAFT_STORE_CREDENTIALS="$(cat /credentials/store.json)"; exec /bin/snapcraft "$@"' \
                snapcraft "$@"
        else
            podman run --rm --entrypoint /bin/sh \
                --volume "$credential_dir:/credentials:ro,Z" "$image" -ec \
                'export SNAPCRAFT_STORE_CREDENTIALS="$(cat /credentials/store.json)"; exec /bin/snapcraft "$@"' \
                snapcraft "$@"
        fi
        ;;
    *) usage ;;
esac
