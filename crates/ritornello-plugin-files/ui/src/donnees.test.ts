import { describe, expect, it } from 'vitest'
import {
  cibleRacine,
  feuille,
  formaterDuree,
  normaliserDonnees,
  normaliserEntrees,
  normaliserRacine,
} from './donnees'

describe('normalisation des racines', () => {
  it('comble les champs que le plugin omet quand ils sont vides', () => {
    // Régression encodée : `Root` est sérialisé avec
    // `skip_serializing_if = "Option::is_none"`, donc `subpath` **disparaît**
    // du corps au lieu d'y figurer à vide. Une vue qui appellerait
    // `r.subpath.trim()` planterait dans un `computed`, et un `computed` qui
    // lève laisse la page à moitié rendue, sans message.
    const r = normaliserRacine({ name: 'nas', kind: 'smb', host: 'h', share: 's' })
    expect(r.subpath).toBe('')
    expect(r.user).toBe('')
    expect(r.writable).toBe(false)
    expect(r.mounted).toBe(false)
  })

  it('traite tout genre inconnu comme un partage', () => {
    expect(normaliserRacine({ kind: 'local' }).kind).toBe('local')
    expect(normaliserRacine({}).kind).toBe('smb')
  })
})

describe('normalisation des niveaux d’arborescence', () => {
  it('accepte les trois formes plausibles rendues par le plugin', () => {
    // Ce champ n'est pas décrit par le contrat écrit, seulement par
    // l'implémentation du plugin. Un lecteur tolérant coûte dix lignes ; une
    // page qui affiche « dossier vide » parce que le serveur a nommé son champ
    // `dirs` au lieu d'`entries` coûte une séance de débogage à travers le
    // socket d'admin.
    const attendu = [
      { name: 'Jazz', path: 'Albums/Jazz', dir: true },
      { name: '01.mp3', path: 'Albums/01.mp3', dir: false },
    ]
    expect(normaliserEntrees(attendu)).toEqual(attendu)
    expect(normaliserEntrees({ entries: attendu })).toEqual(attendu)
    expect(
      normaliserEntrees({
        dirs: [{ name: 'Jazz', path: 'Albums/Jazz' }],
        files: [{ name: '01.mp3', path: 'Albums/01.mp3' }],
      }),
    ).toEqual(attendu)
  })

  it('déduit le nom du chemin quand le plugin ne le donne pas', () => {
    expect(normaliserEntrees({ files: ['Albums/Jazz/01.mp3'] })).toEqual([
      { name: '01.mp3', path: 'Albums/Jazz/01.mp3', dir: false },
    ])
  })

  it('rend une liste vide plutôt que de lever sur un champ absent', () => {
    expect(normaliserEntrees(undefined)).toEqual([])
    expect(normaliserEntrees(null)).toEqual([])
  })
})

describe('normalisation de la charge complète', () => {
  it('accepte un corps minimal sans lever', () => {
    const d = normaliserDonnees({})
    expect(d.roots).toEqual([])
    expect(d.playlist).toEqual([])
    expect(d.scan).toEqual({ running: false, found: 0, dir: '' })
    expect(d.unresolved).toEqual([])
  })

  it('ramène les entrées non résolues à des chemins, quel que soit leur emballage', () => {
    // Elles s'affichent dans un encart : une liste chargée qui rétrécit sans
    // rien dire est un défaut qu'on met des mois à attribuer.
    const d = normaliserDonnees({ unresolved: ['a/b.mp3', { path: 'c/d.flac' }] })
    expect(d.unresolved).toEqual(['a/b.mp3', 'c/d.flac'])
  })

  it('replie une piste sans nom sur le dernier segment de son chemin', () => {
    const d = normaliserDonnees({ playlist: [{ path: 'Albums/Jazz/01.mp3' }] })
    expect(d.playlist[0]).toEqual({
      path: 'Albums/Jazz/01.mp3',
      name: '01.mp3',
      duration_s: 0,
      missing: false,
    })
  })
})

describe('mise en forme', () => {
  it('rend une durée inconnue par un tiret, jamais par « 0:00 »', () => {
    // « 0:00 » affirmerait une piste vide ; le tiret dit qu'on ne sait pas.
    expect(formaterDuree(0)).toBe('—')
    expect(formaterDuree(Number.NaN)).toBe('—')
  })

  it('passe aux heures au-delà de soixante minutes', () => {
    expect(formaterDuree(245)).toBe('4:05')
    expect(formaterDuree(3725)).toBe('1:02:05')
  })

  it('compose la cible d’une racine selon son genre', () => {
    expect(cibleRacine(normaliserRacine({ kind: 'local', path: '/mnt/usb' }))).toBe('/mnt/usb')
    expect(
      cibleRacine(normaliserRacine({ kind: 'smb', host: 'nas', share: 'musique' })),
    ).toBe('//nas/musique')
    expect(
      cibleRacine(
        normaliserRacine({ kind: 'smb', host: 'nas', share: 'musique', subpath: 'Albums' }),
      ),
    ).toBe('//nas/musique/Albums')
  })

  it('extrait le dernier segment d’un chemin', () => {
    expect(feuille('a/b/c.mp3')).toBe('c.mp3')
    expect(feuille('')).toBe('')
  })
})
