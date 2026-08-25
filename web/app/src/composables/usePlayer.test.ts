import { describe, expect, it, vi } from 'vitest'
import type { PlayerPayload } from '../types'
import { formateDuree, formatePosition, riendAfficher, usePlayer } from './usePlayer'

/** Etat complet, dont chaque test retire ce qu'il veut eprouver. */
function etat(partiel: Partial<PlayerPayload> = {}): PlayerPayload {
  return {
    source: 'radio',
    volume: 60,
    muted: false,
    standby: false,
    preset: null,
    preset_count: null,
    preset_name: null,
    status: null,
    overlay: null,
    artist: 'Shaka Ponk',
    title: 'Wanna Get Free',
    album: null,
    duration_s: 214,
    origin: 'ouifm-metas',
    cover_href: null,
    cover_origin: null,
    position_s: null,
    seekable: false,
    can_eject: false,
    ...partiel,
  }
}

/** Faux `EventSource` : retient les instances et permet de pousser des trames. */
class FauxEventSource {
  static instances: FauxEventSource[] = []
  onmessage: ((e: MessageEvent) => void) | null = null
  fermee = false
  constructor(public url: string) {
    FauxEventSource.instances.push(this)
  }
  close() {
    this.fermee = true
  }
  pousse(data: unknown) {
    this.onmessage?.({ data: typeof data === 'string' ? data : JSON.stringify(data) } as MessageEvent)
  }
}

describe('formateDuree', () => {
  it('formate en minutes et secondes', () => {
    expect(formateDuree(214)).toBe('3:34')
    expect(formateDuree(60)).toBe('1:00')
    expect(formateDuree(9)).toBe('0:09')
    expect(formateDuree(3600)).toBe('60:00')
  })

  it('traite comme inconnue toute valeur inexploitable', () => {
    // Ces valeurs viennent d'un tiers : mieux vaut ne rien afficher que « -1:59 ».
    expect(formateDuree(null)).toBeNull()
    expect(formateDuree(undefined)).toBeNull()
    expect(formateDuree(0)).toBeNull()
    expect(formateDuree(-5)).toBeNull()
    expect(formateDuree(Number.NaN)).toBeNull()
    expect(formateDuree(Number.POSITIVE_INFINITY)).toBeNull()
  })
})

describe('formatePosition', () => {
  // `formateDuree` refuse les valeurs <= 0, ce qui est juste pour une duree
  // et faux pour une position : `0:00` est un instant parfaitement legitime.
  // Deux fonctions plutot qu'un assouplissement de la premiere, qui ferait
  // reapparaitre des « 0:00 » la ou le refus servait.
  it('accepte zero', () => {
    expect(formatePosition(0)).toBe('0:00')
  })
  it('formate minutes et secondes', () => {
    expect(formatePosition(87)).toBe('1:27')
    expect(formatePosition(3725)).toBe('62:05')
  })
  it('rend null sur une absence', () => {
    expect(formatePosition(null)).toBeNull()
    expect(formatePosition(undefined)).toBeNull()
    expect(formatePosition(-1)).toBeNull()
    expect(formatePosition(Number.NaN)).toBeNull()
    expect(formatePosition(Number.POSITIVE_INFINITY)).toBeNull()
  })
})

describe('riendAfficher', () => {
  it('accepte toute information partielle', () => {
    // Decision du proprietaire : on affiche tout ce qui est disponible.
    expect(riendAfficher(etat())).toBe(false)
    expect(riendAfficher(etat({ artist: null }))).toBe(false)
    expect(riendAfficher(etat({ title: null }))).toBe(false)
    expect(riendAfficher(etat({ artist: null, title: null, album: 'Kind of Blue' }))).toBe(false)
  })

  it('ne retient ni un etat absent, ni une duree seule', () => {
    expect(riendAfficher(null)).toBe(true)
    expect(riendAfficher(etat({ artist: null, title: null, album: null }))).toBe(true)
    // « 3:34 » sans titre ni artiste n'informe personne.
    expect(riendAfficher(etat({ artist: null, title: null, album: null, duration_s: 214 }))).toBe(true)
  })
})

describe('usePlayer', () => {
  it('ouvre le flux pousse et applique chaque trame', () => {
    FauxEventSource.instances = []
    vi.stubGlobal('EventSource', FauxEventSource)
    const { etat: courant, ouvre, ferme } = usePlayer()
    ouvre()
    const flux = FauxEventSource.instances[0]!
    expect(flux.url).toBe('/api/player')
    expect(courant.value).toBeNull()

    flux.pousse(etat({ title: 'premier' }))
    expect(courant.value?.title).toBe('premier')
    flux.pousse(etat({ title: 'second' }))
    expect(courant.value?.title).toBe('second')

    ferme()
    expect(flux.fermee).toBe(true)
  })

  it('garde l affichage precedent sur une trame illisible', () => {
    // Vider l'ecran parce qu'une trame est corrompue serait plus trompeur que
    // de laisser le morceau precedent une seconde de trop.
    FauxEventSource.instances = []
    vi.stubGlobal('EventSource', FauxEventSource)
    const { etat: courant, ouvre } = usePlayer()
    ouvre()
    const flux = FauxEventSource.instances[0]!
    flux.pousse(etat({ title: 'connu' }))
    flux.pousse('pas du json')
    expect(courant.value?.title).toBe('connu')
  })

  it('ferme le flux precedent plutot que d en accumuler', () => {
    FauxEventSource.instances = []
    vi.stubGlobal('EventSource', FauxEventSource)
    const { ouvre } = usePlayer()
    ouvre()
    ouvre()
    expect(FauxEventSource.instances).toHaveLength(2)
    expect(FauxEventSource.instances[0]!.fermee).toBe(true)
  })

  it('sans EventSource, previent et laisse le reste de la page vivre', () => {
    vi.stubGlobal('EventSource', undefined)
    const avertit = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const { etat: courant, ouvre } = usePlayer()
    expect(() => ouvre()).not.toThrow()
    expect(courant.value).toBeNull()
    expect(avertit).toHaveBeenCalled()
    avertit.mockRestore()
  })
})
