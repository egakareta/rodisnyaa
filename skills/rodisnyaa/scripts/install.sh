#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
skill_dir=$(dirname "$script_dir")
consumer_root=${1:-.}
destination="$consumer_root/.agents/skills/rodisnyaa"

if [ -e "$destination" ]; then
    printf 'Refusing to overwrite existing skill: %s\n' "$destination" >&2
    exit 1
fi

mkdir -p "$(dirname "$destination")"
cp -R "$skill_dir" "$destination"
printf 'Installed rodisnyaa skill at %s\n' "$destination"