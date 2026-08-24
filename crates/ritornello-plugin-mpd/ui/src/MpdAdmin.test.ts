import { toast } from '@ritornello/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import MpdAdmin from './MpdAdmin.vue'

// Meme approche que `ConfigView.test.ts` : on garde le vrai module
// (composants, `api`, ...) et on remplace uniquement les deux entrees de
// `toast` que cette vue utilise, pour pouvoir les observer sans afficher de
// notification.
vi.mock('@ritornello/ui', async () => {
  const reel = await vi.importActual<typeof import('@ritornello/ui')>('@ritornello/ui')
  return { ...reel, toast: { ...reel.toast, error: vi.fn(), success: vi.fn() } }
})

const CATALOGUE = {
  title: 'Serveur MPD',
  listen_label: "Adresse d'écoute",
  port_label: 'Port',
  restart_notice: 'Le changement ne prend effet quau redémarrage du greffon.',
  btn_save: 'Enregistrer',
  saved: 'Enregistré',
  listen_empty: "L'adresse d'écoute ne peut pas être vide.",
  port_zero: 'Le port doit être compris entre 1 et 65535.',
  save_failed: "l'enregistrement a échoué",
  bad_request: 'requête invalide : {detail}',
}

// Prefixe absolu que le shell passe par la prop `base` (requise) : c'est le
// contrat, cette vue ne connait pas le nom sous lequel elle est servie.
const BASE = '/plugins/mpd/'

/** Monte le composant avec un `fetch` espionne servant `donnees` au GET. */
async function monter(donnees: { listen: string; port: number } = { listen: '0.0.0.0', port: 6600 }) {
  const puts: Array<{ url: string; corps: unknown }> = []
  const spy = vi.fn(async (url: string, init?: RequestInit) => {
    if (init?.method === 'PUT') {
      puts.push({ url, corps: JSON.parse(String(init.body)) })
      return new Response(null, { status: 204 })
    }
    return new Response(JSON.stringify(donnees), { status: 200 })
  })
  vi.stubGlobal('fetch', spy)
  const w = mount(MpdAdmin, { props: { catalog: CATALOGUE, base: BASE } })
  await flushPromises()
  return { w, spy, puts }
}

/** Variante ou le PUT est refuse par le serveur (422), comme un vrai refus de validation. */
async function monterAvecRefus(erreur: string) {
  const donnees = { listen: '0.0.0.0', port: 6600 }
  const spy = vi.fn(async (url: string, init?: RequestInit) => {
    if (init?.method === 'PUT') {
      return new Response(JSON.stringify({ error: erreur }), { status: 422 })
    }
    return new Response(JSON.stringify(donnees), { status: 200 })
  })
  vi.stubGlobal('fetch', spy)
  const w = mount(MpdAdmin, { props: { catalog: CATALOGUE, base: BASE } })
  await flushPromises()
  return { w, spy }
}

beforeEach(() => {
  vi.unstubAllGlobals()
  vi.mocked(toast.error).mockClear()
  vi.mocked(toast.success).mockClear()
})

describe('MpdAdmin', () => {
  it('affiche les deux champs avec leurs libelles et les valeurs recues du serveur', async () => {
    const { w } = await monter({ listen: '192.168.1.10', port: 6601 })

    // Les libelles du catalogue apparaissent bien dans le gabarit (et pas une
    // cle brute, qui indiquerait une entree manquante ou un mauvais import).
    expect(w.text()).toContain("Adresse d'écoute")
    expect(w.text()).toContain('Port')

    const champListen = w.get('[data-listen]').element as HTMLInputElement
    const champPort = w.get('[data-port]').element as HTMLInputElement
    // Preuve que la valeur vient bien de la reponse GET, pas des defauts
    // internes (`0.0.0.0` / `6600`) : un test qui laisserait les valeurs par
    // defaut passerait meme si le GET n'etait jamais consomme.
    expect(champListen.value).toBe('192.168.1.10')
    expect(champPort.value).toBe('6601')
  })

  it('envoie les valeurs editees a l enregistrement', async () => {
    const { w, puts } = await monter({ listen: '0.0.0.0', port: 6600 })

    await w.get('[data-listen]').setValue('10.0.0.5')
    await w.get('[data-port]').setValue('6601')
    await w.get('[data-save]').trigger('click')
    await flushPromises()

    expect(puts).toHaveLength(1)
    expect(puts[0]!.url).toBe('/plugins/mpd/api/data')
    // Le port doit etre un nombre dans le corps envoye, pas la chaine tapee
    // dans le champ : `Config` (cote Rust) desattend un entier JSON.
    expect(puts[0]!.corps).toEqual({ listen: '10.0.0.5', port: 6601 })
    expect(toast.success).toHaveBeenCalledWith('Enregistré')
  })

  // Le garde cote client (`aDesErreurs`) est calque exactement sur
  // `Config::valider` : une adresse non vide et un port dans 1..=65535 sont
  // les deux seuls refus que le serveur connait pour ces deux champs, donc
  // un couple qui passe le garde est par construction accepte cote serveur
  // -- il n'existe pas de valeur que le client laisserait passer et que le
  // serveur refuserait *pour ces deux raisons*. Ce test exerce donc un refus
  // qui n'a rien a voir avec la forme des champs : `save_failed`, un echec
  // d'ecriture disque (E/S), qu'aucun garde cote client ne peut anticiper.
  // C'est le seul refus 422 encore atteignable avec une saisie valide.
  it('un refus 422 (echec disque, non detectable cote client) affiche le message traduit du serveur', async () => {
    const { w } = await monterAvecRefus("l'enregistrement a échoué")

    // Saisie parfaitement valide selon le garde client : le refus vient donc
    // bien du serveur, pas d'un blocage local.
    expect((w.get('[data-save]').element as HTMLButtonElement).disabled).toBe(false)
    await w.get('[data-save]').trigger('click')
    await flushPromises()

    // Le message affiche est exactement le texte renvoye par le serveur
    // (deja resolu depuis la cle de catalogue cote Rust) : ni une exception
    // JS, ni une cle brute (`save_failed`).
    expect(toast.error).toHaveBeenCalledWith("l'enregistrement a échoué")
    expect(toast.success).not.toHaveBeenCalled()
  })

  it('un port a 0 marque le champ invalide et desactive Enregistrer : aucun PUT ne part', async () => {
    const { w, spy } = await monter({ listen: '0.0.0.0', port: 6600 })

    await w.get('[data-port]').setValue('0')

    const champPort = w.get('[data-port]')
    const bouton = w.get('[data-save]').element as HTMLButtonElement
    expect(champPort.attributes('aria-invalid')).toBe('true')
    expect(bouton.disabled).toBe(true)
    expect(w.find('[data-port-error]').exists()).toBe(true)

    // `dispatchEvent` plutot que le `trigger()` de VTU : ce dernier renonce
    // de lui-meme sur un element `disabled`, ce qui ferait passer ce test
    // sans qu'aucun garde ne soit exerce dans le code de la vue (voir le
    // meme choix dans `RadioAdmin.test.ts`). On dispatche donc le clic
    // directement : c'est le retour anticipe d'`enregistrer()` qui est
    // teste ici, pas le seul etat visuel du bouton.
    bouton.dispatchEvent(new Event('click'))
    await flushPromises()

    expect(spy.mock.calls.some((c) => (c[1] as RequestInit | undefined)?.method === 'PUT')).toBe(false)
    expect(toast.error).not.toHaveBeenCalled()
    expect(toast.success).not.toHaveBeenCalled()
  })

  it('une adresse vide marque le champ invalide et desactive Enregistrer', async () => {
    const { w } = await monter({ listen: '0.0.0.0', port: 6600 })

    await w.get('[data-listen]').setValue('   ')

    expect(w.get('[data-listen]').attributes('aria-invalid')).toBe('true')
    expect((w.get('[data-save]').element as HTMLButtonElement).disabled).toBe(true)
    expect(w.find('[data-listen-error]').exists()).toBe(true)
  })

  it('avertit du redemarrage necessaire des le chargement, sans attendre un enregistrement', async () => {
    const { w } = await monter()

    // L'avis est present immediatement : le port ne change pas a chaud, donc
    // le lire avant d'agir doit etre possible sans avoir clique sur
    // Enregistrer. Un test qui ne verifierait cela qu'apres un clic sur
    // Enregistrer manquerait la regression que ce cas encode.
    const avis = w.get('[data-restart-notice]')
    expect(avis.text()).toBe('Le changement ne prend effet quau redémarrage du greffon.')
    expect(toast.success).not.toHaveBeenCalled()
    expect(toast.error).not.toHaveBeenCalled()
  })
})
