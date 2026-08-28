#!/bin/sh
# Fake `smbclient` for the end-to-end journey.
#
# The journey has no NAS, and cannot have one: it runs on anyone's machine.
# So it simulates one — but with outputs **captured on a real Synology NAS**
# through samba client 4.19.5, not with a reconstructed format. That is what
# makes the network wizard playable end to end without hardware, while still
# testing the parser against what it will really meet.
#
# The details that matter, and that nobody would have invented:
#   - the administrative share has type "IPC|", not "Disk|";
#   - a noise line "SMB1 disabled" ends the output without failing the command;
#   - attributes are one or two letters ("D" as well as "DA");
#   - one folder name contains spaces.

case "$*" in
  *--version*)
    echo "Version 4.19.5-Ubuntu"
    ;;
  *-L*)
    echo "Disk|music|System default shared folder"
    echo "Disk|photo|System default shared folder"
    echo "IPC|IPC\$|IPC Service ()"
    echo "SMB1 disabled -- no workgroup available"
    ;;
  *-c*ls*)
    echo "  .                                  DA        0  Fri Apr 17 14:46:30 2026"
    echo "  ..                                  D        0  Sun Aug 16 16:23:48 2026"
    echo "  Yann Tiersen                       DA        0  Tue Jul 17 23:07:00 2018"
    echo "  Within Temptation                   D        0  Tue Mar 27 20:20:11 2018"
    echo "  piste.mp3                           A  1234567  Mon Aug 11 20:12:33 2025"
    printf '\n\t\t102400 blocks of size 1024. 102380 blocks available\n'
    ;;
  *)
    # Everything else fails like the real one: exit code 1, message on stderr.
    echo "do_connect: Connection to inconnu failed (Error NT_STATUS_CONNECTION_REFUSED)" >&2
    exit 1
    ;;
esac
