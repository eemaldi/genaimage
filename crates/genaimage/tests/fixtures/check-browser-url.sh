#!/bin/sh
printf 'unexpected launcher output\n'
printf 'unexpected launcher error\n' >&2
[ "$#" -eq 1 ] || exit 1
case "$1" in
    http://127.0.0.1:*/) ;;
    *) exit 1 ;;
esac
case "$1" in
    *'&calc&x'*|*'..'*) exit 1 ;;
esac
