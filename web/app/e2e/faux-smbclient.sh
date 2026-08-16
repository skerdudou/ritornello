#!/bin/sh
# Faux `smbclient` du parcours de bout en bout.
#
# Le parcours n'a pas de NAS, et ne peut pas en avoir : il tourne sur la machine
# de n'importe qui. Il en simule donc un — mais avec les sorties **captées sur
# un vrai NAS Synology** via le client samba 4.19.5, et non avec un format
# reconstitué. C'est ce qui rend l'assistant réseau jouable en entier sans
# matériel, tout en éprouvant l'analyse contre ce qu'elle rencontrera vraiment.
#
# Les détails qui comptent, et qu'on n'aurait pas inventés :
#   - le partage administratif porte le type « IPC| », pas « Disk| » ;
#   - une ligne de bruit « SMB1 disabled » termine la sortie sans faire échouer
#     la commande ;
#   - les attributs tiennent sur une ou deux lettres (« D » comme « DA ») ;
#   - un nom de dossier contient des espaces.

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
    # Tout le reste échoue comme le vrai : code 1, message sur stderr.
    echo "do_connect: Connection to inconnu failed (Error NT_STATUS_CONNECTION_REFUSED)" >&2
    exit 1
    ;;
esac
